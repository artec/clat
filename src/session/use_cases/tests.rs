use super::active::CHECKPOINT_BYTE_CAP;
use super::*;
use crate::session::event::payloads;
use crate::session::replay::ReplayTurnEnd;

fn replay_user(seq: u64, turn: u64, text: &str) -> ReplayEvent {
    ReplayEvent::UserMessage {
        seq,
        turn,
        time_ms: seq as i64,
        text: text.into(),
        content_blocks: Vec::new(),
        client_message_id: None,
        receipt: None,
    }
}

fn replay_assistant(seq: u64, turn: u64, text: &str) -> ReplayEvent {
    ReplayEvent::AssistantMessage {
        seq,
        turn,
        step: 0,
        time_ms: seq as i64,
        reasoning: None,
        text: text.into(),
        tool_calls: Vec::new(),
        provider: "test".into(),
        model: "test".into(),
        replay_state: None,
    }
}

#[test]
fn history_pages_use_exclusive_seq_cursors_and_never_split_a_turn() {
    let replay = vec![
        replay_user(0, 1, "old question"),
        replay_assistant(1, 1, "old answer"),
        ReplayEvent::TurnEnded {
            seq: 2,
            turn: 1,
            time_ms: 2,
            reason: ReplayTurnEnd::Completed,
        },
        replay_user(3, 2, "new question"),
        replay_assistant(4, 2, "new answer"),
        ReplayEvent::TurnEnded {
            seq: 5,
            turn: 2,
            time_ms: 5,
            reason: ReplayTurnEnd::Completed,
        },
    ];

    let tail = paginate_replay(&replay, None, 1);
    assert_eq!(tail.events, replay[3..]);
    assert!(tail.has_more);
    let older = paginate_replay(&replay, Some(3), 1);
    assert_eq!(older.events, replay[..3]);
    assert!(!older.has_more);
    assert!(older.events.iter().all(|event| event.seq() < 3));

    let hostile_mid_turn_cursor = paginate_replay(&replay, Some(5), 50);
    assert_eq!(
        hostile_mid_turn_cursor.events,
        replay[..3],
        "an arbitrary upper cursor cannot split the newer turn"
    );

    let old_cursor_page = paginate_replay(&replay, Some(3), 50);
    let mut compacted = replay.clone();
    compacted.push(ReplayEvent::Compaction {
        seq: 6,
        turn: 2,
        time_ms: 6,
        summary_text: "summary".into(),
    });
    assert_eq!(
        paginate_replay(&compacted, Some(3), 50),
        old_cursor_page,
        "append-only compaction cannot invalidate an older cursor"
    );
}

#[test]
fn armed_replay_is_reused_and_only_a_committed_suffix_is_read() {
    let (service, root) = service("history-replay-cache");
    let summary = service.new_session(&project()).expect("session");
    let key = SessionKey {
        id: summary.id,
        project: project(),
    };
    let journal = service.journal().expect("journal");
    journal
        .append_atomic(&[
            crate::session::run_journal::NewSessionEvent::new(
                "turn/start",
                payloads::turn_start(1),
            )
            .log_only(),
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::user_message("first"),
            )
            .append(Vec::new()),
        ])
        .unwrap();
    journal.flush().unwrap();
    service.quiesce_active().unwrap();
    service.backend.stream_probe.store(0, Ordering::Relaxed);

    service.resume(&key).expect("resume");
    let armed_streams = service.backend.stream_probe.load(Ordering::Relaxed);
    assert!(armed_streams >= 1);
    assert_eq!(
        service
            .history_active(None, 50)
            .unwrap()
            .events
            .iter()
            .filter(|event| event.is_message())
            .count(),
        1
    );
    assert_eq!(service.history_active(Some(1), 50).unwrap().events.len(), 0);
    assert_eq!(
        service.backend.stream_probe.load(Ordering::Relaxed),
        armed_streams,
        "old-page reads reuse the armed prefix"
    );

    let journal = service.journal().expect("journal");
    journal
        .append_atomic(&[
            crate::session::run_journal::NewSessionEvent::new(
                "turn/start",
                payloads::turn_start(2),
            )
            .log_only(),
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::user_message("second"),
            )
            .append(Vec::new()),
        ])
        .unwrap();
    journal.flush().unwrap();
    let page = service.history_active(None, 50).unwrap();
    assert_eq!(
        page.events
            .iter()
            .filter(|event| event.is_message())
            .count(),
        2
    );
    assert_eq!(
        service.backend.stream_probe.load(Ordering::Relaxed),
        armed_streams + 1,
        "freshness reads only the newly committed suffix"
    );
    service.quiesce_active().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn legacy_resume_is_read_only_without_changing_any_session_artifact() {
    let (service, root) = service("legacy-read-only");
    let key = SessionKey {
        project: project(),
        id: SessionId::new("legacy-readable"),
    };
    let mut header = SessionHeader::new(key.id.clone(), key.project.header_cwd.clone(), 1);
    header.version = 0;
    let mut event = SessionEvent::new(
        "user/message",
        0,
        2,
        payloads::user_message("old conversation"),
    );
    event.surface_op = Some(crate::session::event::SurfaceOp::Append);
    let dir = service.backend.create_session_dir(&key).unwrap();
    let bytes =
        crate::session::jsonl::materialized_bytes(&header, &[event], JsonlCompression::Zstd, true)
            .unwrap();
    dir.write("session.jsonl.zstd", &bytes).unwrap();
    let view = service
        .resume(&key)
        .expect("v0 must open through normal resume");
    assert!(!view.replay.is_empty());
    assert!(
        service.journal().is_err(),
        "read-only sessions cannot acquire a run journal"
    );
    service.quiesce_active().unwrap();
    assert_eq!(dir.read("session.jsonl.zstd").unwrap(), bytes);
    assert_eq!(
        dir.entries().unwrap().count(),
        1,
        "no lock, checkpoint, seed or writer artifacts"
    );
    let _ = std::fs::remove_dir_all(root);
}

fn service(tag: &str) -> (SessionService, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "clat-usecases-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    (
        SessionService::new(root.clone(), JsonlCompression::Zstd).expect("service"),
        root,
    )
}

/// MM-I9 / attachment reachability: durable tool results may carry more
/// than one image. Descriptor authority and provider-facing paths are two
/// parallel projections of the same ordered content, so resolving the
/// second opaque id must return the second path, never the first image in
/// the result. The pre-fix implementation searched `image_parts` from the
/// beginning for every descriptor and therefore aliased every id to image
/// zero.
#[test]
fn tool_result_attachment_resolution_preserves_multi_image_pairing() {
    let (service, root) = service("tool-result-image-pairing");
    service.new_session(&project()).expect("session");
    let make_source = |name: &str, color: [u8; 3]| {
        let path = root.join(format!("{name}.png"));
        let image = image::RgbImage::from_pixel(8, 8, image::Rgb(color));
        image::DynamicImage::ImageRgb8(image)
            .save_with_format(&path, image::ImageFormat::Png)
            .expect("write image fixture");
        path
    };
    let images = service
        .import_attachments(&[
            make_source("first", [10, 20, 30]),
            make_source("second", [40, 50, 60]),
        ])
        .expect("admit image pair");
    let blocks = images
        .iter()
        .map(|image| crate::message::ContentBlock::Image {
            attachment: image.descriptor.clone(),
        })
        .collect::<Vec<_>>();
    let journal = service.journal().expect("journal");
    journal
        .append(
            crate::session::run_journal::NewSessionEvent::new(
                "tool/result",
                payloads::tool_result(
                    1,
                    1,
                    "multi-image-call",
                    payloads::tool_result_content_with_blocks(
                        &serde_json::json!("two images"),
                        &blocks,
                    ),
                    false,
                ),
            )
            .append(Vec::new()),
        )
        .expect("append tool result");
    journal.flush().expect("flush tool result");

    let second = service
        .resolve_active_attachment(&images[1].descriptor.attachment_id)
        .expect("resolve second image");
    assert_eq!(second.descriptor, images[1].descriptor);
    assert_eq!(
        second.path, images[1].path,
        "the second opaque id must not alias the first tool image path"
    );
    assert_ne!(second.path, images[0].path);
    std::fs::remove_dir_all(root).ok();
}

/// INV-MM1-3/5: no-follow prevents a final symlink, but a second hardlink
/// name can still mutate the same inode after publication. Provider/PWA
/// reads must therefore reject multiply-linked attachment files instead
/// of treating their regular-file type as sufficient authority.
#[cfg(unix)]
#[test]
fn attachment_reader_rejects_a_multiply_linked_blob() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("clat-attachment-read-link-{unique}"));
    std::fs::create_dir_all(&root).expect("root");
    let blob = root.join("blob");
    let alias = root.join("alias");
    std::fs::write(&blob, b"normalized image bytes").expect("blob");
    std::fs::hard_link(&blob, &alias).expect("hardlink alias");

    assert!(
        open_attachment_file(blob.to_str().expect("utf8 path")).is_err(),
        "a multiply-linked attachment must fail before any bytes are exposed"
    );
    assert_eq!(
        std::fs::read(alias).expect("alias remains intact"),
        b"normalized image bytes"
    );
    std::fs::remove_dir_all(root).ok();
}

/// The PWA/session reader must enforce the same content-address integrity
/// as provider projection. A writable same-user process can alter a 0600
/// inode in place without adding a link or changing its length.
#[test]
fn attachment_reader_rejects_a_content_address_mismatch() {
    use sha2::Digest as _;

    let original = b"original-image";
    let tampered = b"tampered-image";
    assert_eq!(original.len(), tampered.len(), "same-length attack fixture");
    let name = sha2::Sha256::digest(original)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let root = std::env::temp_dir().join(format!(
        "clat-attachment-read-digest-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).expect("root");
    let blob = root.join(name);
    std::fs::write(&blob, tampered).expect("write corrupted blob");

    assert!(
        open_attachment_file(blob.to_str().expect("utf8 path")).is_err(),
        "content-address mismatch must fail before any bytes are exposed"
    );

    std::fs::remove_dir_all(root).ok();
}

/// PWA headers are derived from durable descriptor metadata. Before a
/// blob reader is exposed, that MIME claim must match the normalized blob
/// magic; a content-address match alone cannot authorize relabeling PNG
/// bytes as JPEG.
#[test]
fn active_attachment_reader_rejects_a_media_type_magic_mismatch() {
    let (service, root) = service("attachment-media-mismatch");
    service.new_session(&project()).expect("session");
    let source = root.join("source.png");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([7, 8, 9])))
        .save_with_format(&source, image::ImageFormat::Png)
        .expect("write PNG fixture");
    let mut image = service
        .import_attachments(&[source])
        .expect("admit image")
        .pop()
        .expect("stored image");
    image.descriptor.media_type = "image/jpeg".into();
    let attachment_id = image.descriptor.attachment_id.clone();
    let journal = service.journal().expect("journal");
    journal
        .append(
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::admitted_user_message("m-mime", "", &[image], None, None),
            )
            .append(Vec::new()),
        )
        .expect("append forged durable descriptor");
    journal.flush().expect("flush descriptor");

    let error = match service.open_active_attachment(&attachment_id) {
        Ok(_) => panic!("reader must reject a durable MIME/blob mismatch"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("media type"),
        "failure identifies the descriptor/blob mismatch: {error}"
    );

    std::fs::remove_dir_all(root).ok();
}

/// Durable tool-result descriptors are paired with provider-facing paths
/// during replay, but their byte count is still untrusted journal
/// metadata. The PWA reader must compare it with the already-open blob so
/// a forged descriptor cannot make UI/policy consumers under-report the
/// normalized image while serving different bytes.
#[test]
fn active_attachment_reader_rejects_a_descriptor_byte_count_mismatch() {
    let (service, root) = service("attachment-byte-count-mismatch");
    service.new_session(&project()).expect("session");
    let source = root.join("source.png");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([7, 8, 9])))
        .save_with_format(&source, image::ImageFormat::Png)
        .expect("write PNG fixture");
    let image = service
        .import_attachments(&[source])
        .expect("admit image")
        .pop()
        .expect("stored image");
    let attachment_id = image.descriptor.attachment_id.clone();
    let mut forged = image.descriptor.clone();
    forged.bytes = forged.bytes.saturating_sub(1);
    let journal = service.journal().expect("journal");
    journal
        .append(
            crate::session::run_journal::NewSessionEvent::new(
                "tool/result",
                payloads::tool_result(
                    1,
                    1,
                    "forged-image-metadata",
                    payloads::tool_result_content_with_blocks(
                        &serde_json::json!("image"),
                        &[crate::message::ContentBlock::Image { attachment: forged }],
                    ),
                    false,
                ),
            )
            .append(Vec::new()),
        )
        .expect("append forged durable descriptor");
    journal.flush().expect("flush descriptor");

    let error = match service.open_active_attachment(&attachment_id) {
        Ok(_) => panic!("reader must reject a durable descriptor/blob byte mismatch"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("byte count"),
        "failure identifies the descriptor/blob mismatch: {error}"
    );

    std::fs::remove_dir_all(root).ok();
}

/// Width and height are also durable facts for new content-addressed
/// attachments. A tool-result producer drift must not let the transcript
/// advertise dimensions that do not describe the bytes served by PWA.
#[test]
fn active_attachment_reader_rejects_a_descriptor_dimension_mismatch() {
    let (service, root) = service("attachment-dimension-mismatch");
    service.new_session(&project()).expect("session");
    let source = root.join("source.png");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([7, 8, 9])))
        .save_with_format(&source, image::ImageFormat::Png)
        .expect("write PNG fixture");
    let image = service
        .import_attachments(&[source])
        .expect("admit image")
        .pop()
        .expect("stored image");
    let attachment_id = image.descriptor.attachment_id.clone();
    let mut forged = image.descriptor.clone();
    forged.width = forged.width.saturating_add(1);
    let journal = service.journal().expect("journal");
    journal
        .append(
            crate::session::run_journal::NewSessionEvent::new(
                "tool/result",
                payloads::tool_result(
                    1,
                    1,
                    "forged-image-dimensions",
                    payloads::tool_result_content_with_blocks(
                        &serde_json::json!("image"),
                        &[crate::message::ContentBlock::Image { attachment: forged }],
                    ),
                    false,
                ),
            )
            .append(Vec::new()),
        )
        .expect("append forged durable descriptor");
    journal.flush().expect("flush descriptor");

    let error = match service.open_active_attachment(&attachment_id) {
        Ok(_) => panic!("reader must reject a durable descriptor/blob dimension mismatch"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("dimensions"),
        "failure identifies the descriptor/blob mismatch: {error}"
    );

    std::fs::remove_dir_all(root).ok();
}

/// Digest verification and HTTP streaming cannot be two reads of a
/// writable inode: a same-user process can change that inode after the
/// first pass and make the browser receive bytes that were never
/// verified. The application boundary therefore returns an immutable,
/// bounded snapshot rather than the live store descriptor.
#[test]
fn active_attachment_reader_streams_the_verified_snapshot_after_blob_mutation() {
    use std::io::Read as _;

    let (service, root) = service("attachment-immutable-snapshot");
    service.new_session(&project()).expect("session");
    let source = root.join("source.png");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([7, 8, 9])))
        .save_with_format(&source, image::ImageFormat::Png)
        .expect("write PNG fixture");
    let image = service
        .import_attachments(&[source])
        .expect("admit image")
        .pop()
        .expect("stored image");
    let attachment_id = image.descriptor.attachment_id.clone();
    let journal = service.journal().expect("journal");
    journal
        .append(
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::admitted_user_message(
                    "m-snapshot",
                    "",
                    std::slice::from_ref(&image),
                    None,
                    None,
                ),
            )
            .append(Vec::new()),
        )
        .expect("append durable descriptor");
    journal.flush().expect("flush descriptor");
    let original = std::fs::read(&image.path).expect("read admitted blob");

    let mut reader = service
        .open_active_attachment(&attachment_id)
        .expect("open verified attachment snapshot");
    let mut mutated = original.clone();
    let last = mutated.last_mut().expect("non-empty PNG");
    *last ^= 0xff;
    std::fs::write(&image.path, &mutated).expect("mutate store inode after verification");

    let mut exposed = Vec::new();
    reader
        .file
        .read_to_end(&mut exposed)
        .expect("consume application reader");
    assert_eq!(
        exposed, original,
        "browser bytes must be the exact snapshot that passed digest verification"
    );

    std::fs::remove_dir_all(root).ok();
}

/// INV-MM1-4 + MM-I9: `view_image` stores its descriptor inside the
/// nested DSH tool-result content, not a top-level user message. The cold
/// orphan mark must protect that blob while still reclaiming a genuinely
/// unreferenced peer. Removing the `tool/result` collector makes the
/// referenced blob disappear in this test.
#[test]
fn orphan_mark_preserves_tool_result_image_attachments() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("clat-tool-image-gc-{unique}"));
    let store = crate::session::attachments::AttachmentStore::open(root.clone())
        .expect("open attachment store");
    let make_source = |name: &str, color: [u8; 3]| {
        let path = root.join(format!("{name}.png"));
        let image = image::RgbImage::from_pixel(8, 8, image::Rgb(color));
        image::DynamicImage::ImageRgb8(image)
            .save_with_format(&path, image::ImageFormat::Png)
            .expect("write image fixture");
        path
    };
    let referenced_source = make_source("referenced", [10, 20, 30]);
    let orphan_source = make_source("orphan", [40, 50, 60]);
    let stored = store
        .admit(&[referenced_source, orphan_source])
        .expect("admit image pair");
    let referenced_image = crate::message::AttachmentDescriptor {
        attachment_id: stored[0].id.clone(),
        media_type: stored[0].media_type.to_owned(),
        width: stored[0].width,
        height: stored[0].height,
        bytes: stored[0].bytes,
        display_name: stored[0].display_name.clone(),
        original_width: Some(stored[0].original_width),
        original_height: Some(stored[0].original_height),
    };
    let event = SessionEvent::new(
        "tool/result",
        0,
        0,
        payloads::tool_result(
            1,
            1,
            "view-image-call",
            payloads::tool_result_content_with_blocks(
                &serde_json::json!("ok"),
                &[crate::message::ContentBlock::Image {
                    attachment: referenced_image,
                }],
            ),
            false,
        ),
    );
    let mut referenced = std::collections::HashSet::new();
    collect_event_attachment_ids(&event, &mut referenced);
    assert_eq!(
        referenced,
        std::collections::HashSet::from([stored[0].id.clone()])
    );

    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(48 * 60 * 60);
    let sweep = store.sweep_orphans(&referenced, future);
    assert_eq!(sweep.removed_blobs, 1, "the unrelated orphan is reclaimed");
    assert!(
        std::path::Path::new(&stored[0].blob_path).is_file(),
        "the durable tool-result image remains readable"
    );
    assert!(
        !std::path::Path::new(&stored[1].blob_path).exists(),
        "the unreferenced control blob proves the sweep actually ran"
    );
    std::fs::remove_dir_all(root).ok();
}

/// INV-MM2-6（MM-2 W6 红测）：相对 ref `blobs/<hex>` 在栅栏处
/// 解析为会话 root 内绝对路径；畸形 ref（空 id/非十六进制/多
/// 组件）不解析且按围栏语义占位。删 resolve_blob_reference 即红
///（合法 ref 无法解析 → 围栏拒绝 → 占位）。
#[test]
fn fence_resolves_blob_references_within_the_root() {
    let root = std::path::PathBuf::from("/store/sessions/p/s/attachments");
    let mut item = crate::model::ModelItem::User {
        content: vec![
            crate::model::ContentPart::Text("look".into()),
            crate::model::ContentPart::Image {
                path: "blobs/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .into(),
                media_type: "image/png".into(),
            },
        ],
    };
    fence_attachment_parts(&mut item, &root);
    let crate::model::ModelItem::User { content } = &item else {
        panic!("user item");
    };
    let crate::model::ContentPart::Image { path, .. } = &content[1] else {
        panic!("the valid ref must resolve, not placeholder");
    };
    assert_eq!(
        path,
        &root
            .join("blobs")
            .join("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .to_string_lossy()
            .into_owned()
    );

    // 畸形 ref：非十六进制 id / 多组件 / 绝对越界路径 → 占位。
    for malformed in ["blobs/not-hex!", "blobs/aaaa/bbbb", "blobs/", "/etc/passwd"] {
        let mut item = crate::model::ModelItem::User {
            content: vec![crate::model::ContentPart::Image {
                path: malformed.into(),
                media_type: "image/png".into(),
            }],
        };
        fence_attachment_parts(&mut item, &root);
        let crate::model::ModelItem::User { content } = &item else {
            panic!("user item");
        };
        assert!(
            matches!(content[0], crate::model::ContentPart::Text(ref note) if note.contains("image unavailable")),
            "malformed reference {malformed} degrades to a stable placeholder"
        );
    }
}

/// MM-1 围栏五腿判别（git checkout 事故丢失，2026-08-27 研究员
/// 按 MM-1 审查记录还原；规格见 mm2 实施计划 §事故记档）：
/// ①root 内 legacy 平铺文件保留 ②blobs/ 内合法引用保留
/// ③`..` 词法逃逸占位 ④root 内 symlink 绝不跟随 ⑤界外绝对
/// 路径占位。删 path_is_within_attachment_root 任一闸即红。
#[test]
fn fence_rejects_paths_outside_the_session_store() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("clat-fence-{unique}"));
    std::fs::create_dir_all(root.join("blobs")).expect("blobs dir");

    std::fs::write(root.join("flat.png"), b"legacy").expect("flat file");
    std::fs::write(root.join("blobs").join("deadbeef"), b"blob").expect("blob file");

    let fenced = |path: String| -> crate::model::ContentPart {
        let mut item = crate::model::ModelItem::User {
            content: vec![crate::model::ContentPart::Image {
                path,
                media_type: "image/png".into(),
            }],
        };
        fence_attachment_parts(&mut item, &root);
        let crate::model::ModelItem::User { content } = &item else {
            panic!("user item");
        };
        content.first().expect("one part").clone()
    };

    // 腿 ①②：root 内平铺（legacy）与 blobs/ 内引用照常保留。
    for keep in [
        root.join("flat.png").to_string_lossy().into_owned(),
        root.join("blobs")
            .join("deadbeef")
            .to_string_lossy()
            .into_owned(),
    ] {
        match fenced(keep.clone()) {
            crate::model::ContentPart::Image { path, .. } => {
                assert_eq!(path, keep, "in-root references must stay readable");
            }
            other => panic!("in-root reference must stay an image part, got {other:?}"),
        }
    }

    // 腿 ③：`..` 组件词法逃逸（下溢 / 弹出 root）→ 稳定占位。
    for escape in [
        root.join("..")
            .join("secret.png")
            .to_string_lossy()
            .into_owned(),
        root.join("blobs")
            .join("..")
            .join("..")
            .join("x.png")
            .to_string_lossy()
            .into_owned(),
    ] {
        assert!(
            matches!(fenced(escape), crate::model::ContentPart::Text(ref note) if note.contains("image unavailable")),
            "a `..` escape must degrade to a stable placeholder"
        );
    }

    // 腿 ④：root 内 symlink 指向界外 → 拒绝（词法在界内也不跟随）。
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(std::env::temp_dir(), root.join("link")).expect("symlink");
        let linked = root
            .join("link")
            .join("outside.png")
            .to_string_lossy()
            .into_owned();
        assert!(
            matches!(fenced(linked), crate::model::ContentPart::Text(ref note) if note.contains("image unavailable")),
            "a symlink inside the root must never be followed"
        );
        let _ = std::fs::remove_file(root.join("link"));
    }

    // 腿 ⑤：界外绝对路径 → 占位（词法栅栏，无需目标存在）。
    assert!(
        matches!(fenced("/etc/passwd".into()), crate::model::ContentPart::Text(ref note) if note.contains("image unavailable")),
        "an absolute path outside the store must degrade to a stable placeholder"
    );

    let _ = std::fs::remove_dir_all(&root);
}

fn project() -> ProjectKey {
    ProjectKey::from_cwd("/tmp/usecases")
}

fn run_turn(service: &SessionService, text: &str) -> Result<(), SessionError> {
    let journal = service.journal()?;
    journal
        .append_atomic(&[
            crate::session::run_journal::NewSessionEvent::new(
                "turn/start",
                payloads::turn_start(1),
            ),
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::user_message(text),
            )
            .append(Vec::new()),
            crate::session::run_journal::NewSessionEvent::new(
                "turn/end",
                payloads::turn_end(1, &crate::session::event::TurnEndReason::Completed),
            ),
        ])
        .map_err(SessionError::Corruption)?;
    journal.flush().map_err(SessionError::Corruption)?;
    Ok(())
}

#[test]
fn global_admission_owner_scan_outlives_receipt_window_and_rejects_duplicates() {
    let (service, root) = service("global-admission-owner");
    let project = project();
    let first = service.new_session(&project).expect("first session");
    let journal = service.journal().expect("journal");
    let digest = "a".repeat(64);
    let mut events = vec![
        crate::session::run_journal::NewSessionEvent::new(
            "user/message",
            payloads::admitted_user_message(
                "message-target",
                "target",
                &[],
                Some("delivery-target"),
                Some(&digest),
            ),
        )
        .append(Vec::new()),
    ];
    for index in 0..1024 {
        events.push(
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::admitted_user_message(
                    &format!("message-{index}"),
                    "filler",
                    &[],
                    Some(&format!("delivery-{index}")),
                    Some(&digest),
                ),
            )
            .append(Vec::new()),
        );
    }
    journal
        .append_atomic(&events)
        .expect("append admission window");
    journal.flush().expect("commit admission window");
    assert!(
        service.committed_admission("delivery-target").is_none(),
        "the ordinary retry projection is intentionally bounded"
    );
    let owner = service
        .find_committed_admission_session(&project, "delivery-target")
        .expect("scan journals")
        .expect("historical owner");
    assert_eq!(owner.0, first.id);
    assert_eq!(owner.1.request_digest.as_deref(), Some(digest.as_str()));

    service.quiesce_active().expect("quiesce first");
    service.new_session(&project).expect("second session");
    let journal = service.journal().expect("second journal");
    journal
        .append(
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::admitted_user_message(
                    "duplicate-message",
                    "duplicate",
                    &[],
                    Some("delivery-target"),
                    Some(&digest),
                ),
            )
            .append(Vec::new()),
        )
        .expect("append duplicate owner");
    journal.flush().expect("commit duplicate owner");
    let error = service
        .find_committed_admission_session(&project, "delivery-target")
        .expect_err("multiple durable owners must fail closed");
    assert!(error.to_string().contains("multiple project sessions"));

    service.quiesce_active().expect("cleanup");
    crate::test_support::cleanup_tree(&root);
}

struct FailingPlanJournal {
    append_error: bool,
    flush_error: bool,
}

impl crate::session::run_journal::RunJournal for FailingPlanJournal {
    fn append_atomic(
        &self,
        _events: &[crate::session::run_journal::NewSessionEvent],
    ) -> Result<crate::session::run_journal::SeqRange, String> {
        if self.append_error {
            Err("intentional plan append failure".into())
        } else {
            Ok(crate::session::run_journal::SeqRange {
                start: 99,
                end_inclusive: 99,
            })
        }
    }

    fn flush(&self) -> Result<(), String> {
        if self.flush_error {
            Err("intentional plan flush failure".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn plan_mode_append_and_flush_failures_never_publish_approval() {
    let (service, root) = service("plan-commit-failures");
    service.new_session(&project()).expect("session");
    service
        .record_plan_mode(true, None)
        .expect("enter plan mode");
    assert!(service.plan_mode_state().active);

    let replace_journal = |journal: Arc<dyn crate::session::run_journal::RunJournal>| {
        let guard = service.active.lock().expect("active");
        let active = guard.as_ref().expect("active session");
        active.replace_journal_for_test(Some(journal));
    };

    replace_journal(Arc::new(FailingPlanJournal {
        append_error: true,
        flush_error: false,
    }));
    let approved = ApprovedPlanWrite {
        text: "approved only after durable commit".into(),
        digest: crate::plan_mode::plan_digest("approved only after durable commit"),
    };
    assert!(
        service
            .record_plan_mode(false, Some(approved.clone()))
            .is_err()
    );
    assert!(service.plan_mode_state().active);
    assert!(service.plan_mode_state().approved.is_none());

    replace_journal(Arc::new(FailingPlanJournal {
        append_error: false,
        flush_error: true,
    }));
    assert!(service.record_plan_mode(false, Some(approved)).is_err());
    assert!(service.plan_mode_state().active);
    assert!(service.plan_mode_state().approved.is_none());

    // Restore the real folding journal so normal teardown can flush/close.
    {
        let guard = service.active.lock().expect("active");
        let active = guard.as_ref().expect("active session");
        active.replace_journal_for_test(None);
    }
    service.quiesce_active().expect("quiesce");
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn plan_mode_checkpoint_failure_after_commit_is_rebuildable_degradation() {
    let (service, root) = service("plan-checkpoint-degradation");
    let project = project();
    let summary = service.new_session(&project).expect("session");
    let key = SessionKey {
        project: project.clone(),
        id: summary.id,
    };
    service
        .record_plan_mode(true, None)
        .expect("enter plan mode");
    service
        .fail_next_plan_checkpoint
        .store(true, Ordering::Release);
    let text = "checkpoint failure must not undo a durable approval";
    let seq = service
        .record_plan_mode(
            false,
            Some(ApprovedPlanWrite {
                text: text.into(),
                digest: crate::plan_mode::plan_digest(text),
            }),
        )
        .expect("checkpoint failure is non-fatal")
        .expect("durable approval seq");
    let state = service.plan_mode_state();
    assert!(!state.active);
    assert_eq!(
        state.approved.as_ref().map(|plan| plan.event_seq),
        Some(seq)
    );
    assert_eq!(
        state.approved.as_ref().map(|plan| plan.text.as_str()),
        Some(text)
    );
    service.quiesce_active().expect("quiesce");
    drop(service);

    let reopened = SessionService::new(root.clone(), JsonlCompression::Zstd).expect("reopen");
    reopened
        .resume(&key)
        .expect("resume from stale checkpoint + journal tail");
    let rebuilt = reopened.plan_mode_state();
    assert!(!rebuilt.active);
    assert_eq!(
        rebuilt.approved.as_ref().map(|plan| plan.event_seq),
        Some(seq)
    );
    assert_eq!(
        rebuilt.approved.as_ref().map(|plan| plan.text.as_str()),
        Some(text)
    );
    reopened.quiesce_active().expect("reopen quiesce");
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn permission_mode_checkpoint_failure_after_commit_rebuilds_from_journal() {
    let (service, root) = service("permission-checkpoint-degradation");
    let project = project();
    let summary = service.new_session(&project).expect("session");
    let key = SessionKey {
        project,
        id: summary.id,
    };
    service
        .record_permission_mode(PermissionMode::ProjectWrite)
        .expect("initial mode");
    service
        .fail_next_permission_checkpoint
        .store(true, Ordering::Release);
    service
        .record_permission_mode(PermissionMode::FullAccess)
        .expect("checkpoint failure is non-fatal after journal commit");
    assert_eq!(
        service.permission_mode_state(),
        Some(PermissionMode::FullAccess)
    );
    service.quiesce_active().expect("quiesce");
    drop(service);

    let reopened = SessionService::new(root.clone(), JsonlCompression::Zstd).expect("reopen");
    reopened.resume(&key).expect("rebuild from journal tail");
    assert_eq!(
        reopened.permission_mode_state(),
        Some(PermissionMode::FullAccess)
    );
    reopened.quiesce_active().expect("quiesce reopened");
    crate::test_support::cleanup_tree(&root);
}

/// 启动加载的分阶段计时（诊断用，`--ignored` 运行）。模拟一个真实
/// 长会话的日志形状：多轮 ×（用户消息 + 助手回复 + 带内容的工具
/// 调用/结果），然后分别计时 resume 流水线的三个完整日志遍：
/// 恢复扫描（prepare）、checkpoint 之后的投影折叠（visit_from）、
/// 前端转录重建（replay_with_usage）。数字用于指导"合并遍数"类
/// 优化；事件体量刻意接近 dogfood 会话（工具结果数 KB）。
#[test]
#[ignore = "diagnostic timing bench, run with --nocapture"]
fn large_journal_resume_phase_timing() {
    let (service, root) = service("resume-timing");
    let summary = service.new_session(&project()).expect("session");
    let key = SessionKey {
        id: summary.id.clone(),
        project: project(),
    };
    let tool_content = "x".repeat(4 * 1024);
    let stream_texts = (0..160)
        .map(|index| format!("stream-{index:03}-{}", "x".repeat(24)))
        .collect::<Vec<_>>();
    let stream_dt = vec![1; stream_texts.len() - 1];
    let turns = 400u64;
    for turn in 0..turns {
        let journal = service.journal().expect("journal");
        let usage = crate::model::Usage {
            input_tokens: 32000,
            output_tokens: 900,
            cached_input_tokens: Some(24000),
            reasoning_tokens: None,
        };
        let step = turn + 1;
        let mut assistant = payloads::assistant_message(
            step,
            step,
            vec![payloads::text_block(&format!(
                "turn {turn} plan:\n- read the module\n- patch\n- verify"
            ))],
            "deepseek",
            "deepseek-v4-pro",
            Some(&usage),
        );
        assistant["stream"] = serde_json::json!([
            {
                "type": "reasoning-chunks",
                "time0": 1_000,
                "index": 0,
                "dt": stream_dt,
                "texts": stream_texts,
            },
            {
                "type": "chunk",
                "time": 1_160,
                "chunk": {"type": "finish", "reason": {"kind": "stop"}},
            }
        ]);
        journal
            .append_atomic(&[
                crate::session::run_journal::NewSessionEvent::new(
                    "turn/start",
                    payloads::turn_start(step),
                ),
                crate::session::run_journal::NewSessionEvent::new(
                    "user/message",
                    payloads::user_message(&format!("turn {turn}: please inspect and fix")),
                )
                .append(Vec::new()),
                crate::session::run_journal::NewSessionEvent::new("assistant/message", assistant)
                    .append(Vec::new()),
                crate::session::run_journal::NewSessionEvent::new(
                    "tool/call",
                    payloads::tool_call(
                        step,
                        step,
                        &format!("call-{turn}"),
                        "read_file",
                        &serde_json::json!({ "path": "src/lib.rs" }),
                    ),
                ),
                crate::session::run_journal::NewSessionEvent::new(
                    "tool/result",
                    payloads::tool_result(
                        step,
                        step,
                        &format!("call-{turn}"),
                        payloads::tool_result_content(&serde_json::json!(tool_content)),
                        false,
                    ),
                )
                .append(Vec::new()),
                crate::session::run_journal::NewSessionEvent::new(
                    "turn/end",
                    payloads::turn_end(step, &crate::session::event::TurnEndReason::Completed),
                ),
            ])
            .expect("append turn");
    }
    // 刷新一次写出全部待写批次，并让 checkpoint 站在日志末尾——
    // 计时的是"最近一次干净关闭后重开"的冷启动路径。
    service
        .journal()
        .expect("journal")
        .flush()
        .expect("flush pending batches");
    {
        let guard = service.active.lock().expect("active");
        let active = guard.as_ref().expect("armed");
        checkpoint_active(active, &service.checkpoints).expect("checkpoint");
    }
    let log_size = dir_size(&root);
    eprintln!(
        "journal: {turns} turns, {} bytes ({:.1} MiB)",
        log_size,
        log_size as f64 / 1024.0 / 1024.0
    );
    // 干净关闭：租约不变量（DV-2/3）下同一会话只允许一个写者，
    // 服务层同键 resume 会先 quiesce 再重挂。本计时腿声称测的正是
    // 「最近一次干净关闭后重开」——写者必须先退场，直调
    // backend.prepare 才是冷重开路径（active 写者未退场时 prepare
    // 会被租约以 Conflict 拒绝，CI 2026-09-07 门控步红即此）。
    service
        .quiesce_active()
        .expect("quiesce for the cold reopen");

    let time = |label: &str, f: &mut dyn FnMut()| {
        let start = std::time::Instant::now();
        f();
        eprintln!("{label}: {:?}", start.elapsed());
    };

    // 阶段 1：prepare 的全量恢复扫描（start_unseeded 内部路径）。
    time("prepare scan (full pass)", &mut || {
        service.backend.prepare(&key).expect("prepare");
    });
    // 阶段 2：checkpoint 命中时的投影折叠（floor 之后应近零）。
    time("projection fold from checkpoint", &mut || {
        service
            .backend
            .visit_from(&key, 0, &mut |_| Ok(()))
            .expect("visit");
    });
    // 阶段 3：前端转录重建（replay，永远从 seq 0）。
    time("replay_with_usage (full pass)", &mut || {
        let (replay, usage) = service.replay_with_usage(&key).expect("replay");
        assert!(!replay.is_empty());
        assert!(usage.session.input_tokens > 0);
    });
    // 整体：一次真实 resume（= 单遍流读 + 协调器开销）。
    time("full resume()", &mut || {
        service.resume(&key).expect("resume");
    });
    // 泄漏纪律同上：quiesce 后再清理临时目录。
    service.quiesce_active().expect("quiesce");
    std::fs::remove_dir_all(&root).ok();
}

/// 递归累加目录字节数（诊断用：报告日志体量）。
fn dir_size(root: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                total += dir_size(&entry.path());
            } else if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }
    total
}

/// 不变量 R-1（2026-08-19 启动性能，DSH 对照）：干净日志的冷
/// resume 对日志恰好发起**一次**物理流读——prepare 的恢复扫描
/// 同时完成投影折叠、转录回放与 usage 统计。torn-tail 崩溃修复
/// 路径另行允许丢弃重读。pre-fix 为 3 遍（prepare 扫描 + 投影
/// 折叠 + replay），日志体量线性放大后即用户可感的启动延迟。
#[test]
fn cold_resume_streams_the_log_exactly_once() {
    let (service, root) = service("single-pass");
    let summary = service.new_session(&project()).expect("session");
    let key = SessionKey {
        id: summary.id.clone(),
        project: project(),
    };
    for i in 0..3 {
        run_turn(&service, &format!("turn {i}")).expect("turn");
    }
    service.journal().expect("journal").flush().expect("flush");
    let before = service.stream_probe();
    let view = service.resume(&key).expect("cold resume");
    let streams = service.stream_probe() - before;
    assert_eq!(
        streams, 1,
        "a clean cold resume must stream the log exactly once (got {streams})"
    );
    assert!(
        !view.replay.is_empty(),
        "the replay was built in that one pass"
    );
    // resume() 安装了活动会话：不 quiesce 就结束会泄漏一个 writer
    // 线程，并行套件里抬高的全局计数会把相邻的线程回收测试顶红
    //（a_hundred_session_switches 实测）。下面的计时诊断同因，也
    // 必须在清理前 quiesce。
    service.quiesce_active().expect("quiesce");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn checkpoint_budget_never_snapshots_unbounded_event_units() {
    let mut registry = ProjectionRegistry::clat();
    registry
        .fold_all(&[SessionEvent::new(
            "user/message",
            0,
            1,
            payloads::user_message(&"x".repeat(4 * 1024 * 1024)),
        )
        .append(Vec::new())])
        .expect("fold huge event");
    let bounded = registry.checkpoint_bounded(
        CheckpointIdentity {
            created_at: 1,
            cwd: Some("/tmp/usecases".into()),
        },
        7,
        CHECKPOINT_BYTE_CAP,
    );
    assert!(serde_json::to_vec_pretty(&bounded).unwrap().len() <= CHECKPOINT_BYTE_CAP);
    assert_eq!(bounded.generation, 7);
    assert!(!bounded.rows.contains_key("surface"));
    assert!(!bounded.rows.contains_key("transcript"));
}

#[test]
fn checkpoint_generation_advances_on_each_publish() {
    let (service, root) = service("generation");
    let project = project();
    let summary = service.new_session(&project).expect("new");
    run_turn(&service, "hello").expect("run");
    let key = SessionKey {
        project,
        id: summary.id,
    };
    service.sync_active().expect("checkpoint 1");
    let first = service.checkpoints.load(&key).expect("first checkpoint");
    service.sync_active().expect("checkpoint 2");
    let second = service.checkpoints.load(&key).expect("second checkpoint");
    assert!(second.generation > first.generation);
    service.quiesce_active().expect("close");
    crate::test_support::cleanup_tree(&root);
}

/// 回归（真实事故）：重启后第一次启动以 "changed while streaming" 失败，
/// 第二次成功——install_armed 把 resume seed 留在 write-behind 车道里，
/// 而挂载期 snapshot() 的全量流式读把它的落盘当成了外部写入者。
/// 不变量：install 返回时 seed 必须已经持久化（读屏障，见
/// [`SessionCoordinator::flush`]）。修复前该测试失败：marker 最长
/// 200ms 后落盘，而这里的磁盘读取发生在微秒级。
#[test]
fn installed_resume_seed_is_durable_before_install_returns() {
    let (service, root) = service("seed-durable");
    let project = project();
    let summary = service.new_session(&project).expect("new");
    run_turn(&service, "hello").expect("turn");
    let key = SessionKey {
        project: project.clone(),
        id: summary.id.clone(),
    };
    // 退役现有 writer；日志末事件是 turn/end 而非 end-seed，
    // 下一次 prepare 因此需要 seed marker。
    service.quiesce_active().expect("quiesce");

    let staged = service.stage_resume(&key).expect("stage");
    let armed = service.arm_session(staged).expect("arm");
    let _view = service.install_armed(armed);

    // 用独立 backend 观察磁盘真值（绕过本进程任何内存状态）。
    let observer = JsonlBackend::new(root.clone(), JsonlCompression::Zstd, false);
    let events = observer.load(&key, false).expect("durable read").events;
    assert_eq!(
        events.last().expect("session has events").event_type,
        "session/end-seed",
        "install_armed must make the resume seed durable before returning"
    );
    service.quiesce_active().expect("cleanup");
    crate::test_support::cleanup_tree(&root);
}

/// I4：回放是事件日志的纯折叠——删 checkpoint、走完整 resume 冷读
/// 阶梯，结果必须逐项相等。
#[test]
fn replay_is_a_pure_log_fold_checkpoints_change_nothing() {
    let (service, root) = service("replay-checkpoints");
    let project = project();
    let summary = service.new_session(&project).expect("new");
    run_turn(&service, "hello").expect("run 1");
    run_turn(&service, "again").expect("run 2");
    service.sync_active().expect("checkpoint");
    let key = SessionKey {
        project: project.clone(),
        id: summary.id.clone(),
    };
    let direct = service.replay(&key).expect("replay with checkpoint");
    // Shape sanity before the equivalence claims: two user turns, each
    // explained by a completed turn/end (time/turn metadata varies, so
    // project to the essentials).
    let essentials = |items: &[ReplayEvent]| {
        items
            .iter()
            .map(|item| match item {
                ReplayEvent::UserMessage { text, .. } => format!("user:{text}"),
                ReplayEvent::TurnEnded { reason, .. } => format!("end:{reason:?}"),
                other => format!("other:{other:?}"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        essentials(&direct),
        vec![
            "user:hello".to_owned(),
            "end:Completed".to_owned(),
            "user:again".to_owned(),
            "end:Completed".to_owned(),
        ]
    );

    // A lazy session with no log replays empty (never an error).
    service.checkpoints.drop(&key);
    // Arming a resume target opens a writable prepared handle. The
    // active coordinator must be retired first; staging itself remains
    // read-only and can still be performed before this boundary.
    service.quiesce_active().expect("detach before arm");
    let staged = service.stage_resume(&key).expect("stage");
    let armed = service.arm_session(staged).expect("arm");
    assert_eq!(&armed.view.replay, &direct, "resume without checkpoint");
    service.discard_armed(armed).expect("discard");
    assert!(
        service
            .replay(&SessionKey {
                project,
                id: SessionId::generate()
            })
            .unwrap()
            .is_empty(),
        "a session without a log replays empty"
    );
    crate::test_support::cleanup_tree(&root);
}

/// I5：崩溃残留（open step + 无 result 的 tool/call）经恢复闭合器
/// 补齐后，合成事件如常回放——工具名照常配对。
#[test]
fn interrupted_log_replays_recovery_synthetic_closers() {
    let (service, root) = service("replay-interrupted");
    let project = project();
    let summary = service.new_session(&project).expect("new");
    let journal = service.journal().expect("journal");
    journal
        .append_atomic(&[
            crate::session::run_journal::NewSessionEvent::new(
                "turn/start",
                payloads::turn_start(1),
            ),
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::user_message("do things"),
            )
            .append(Vec::new()),
            crate::session::run_journal::NewSessionEvent::new(
                "step/start",
                payloads::step_start(1, 0),
            ),
            // Real producers always announce the call in the settled
            // assistant message before the durable tool/call (recovery
            // registers pending calls from these blocks).
            crate::session::run_journal::NewSessionEvent::new(
                "assistant/message",
                payloads::assistant_message(
                    1,
                    0,
                    vec![payloads::tool_call_block(
                        "call-crash",
                        "read_file",
                        &serde_json::json!({"path": "x"}),
                    )],
                    "application-test",
                    "deterministic",
                    None,
                ),
            )
            .append(Vec::new()),
            crate::session::run_journal::NewSessionEvent::new(
                "tool/call",
                payloads::tool_call(
                    1,
                    0,
                    "call-crash",
                    "read_file",
                    &serde_json::json!({"path": "x"}),
                ),
            )
            .log_only(),
        ])
        .map_err(SessionError::Corruption)
        .expect("interrupted batch");
    journal
        .flush()
        .map_err(SessionError::Corruption)
        .expect("durable");
    service.quiesce_active().expect("close");

    let key = SessionKey {
        project,
        id: summary.id,
    };
    let staged = service.stage_resume(&key).expect("stage");
    let armed = service.arm_session(staged).expect("arm repairs the log");
    let replay = armed.view.replay.clone();
    // arm performs recovery before the view is built, so the synthetic
    // isError tool/result and interrupted turn/end replay like any other
    // producer event.
    assert!(
        replay.iter().any(|item| matches!(item,
            ReplayEvent::ToolFinished { call_id, tool, is_error, .. }
            if call_id == "call-crash" && tool == "read_file" && *is_error)),
        "synthetic outcome-unknown tool result must replay, paired by callId: {replay:?}"
    );
    assert!(
        replay.iter().any(|item| matches!(
            item,
            ReplayEvent::TurnEnded {
                reason: ReplayTurnEnd::Interrupted,
                ..
            }
        )),
        "the interrupted turn must explain its stop: {replay:?}"
    );
    service.discard_armed(armed).expect("discard");
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn new_session_never_overwrites_an_unquiesced_active_writer() {
    let (service, root) = service("new-overwrite");
    service.new_session(&project()).expect("first");
    assert!(matches!(
        service.new_session(&project()),
        Err(SessionError::Conflict(message)) if message.contains("quiescing")
    ));
    service.quiesce_active().expect("close first");
    assert!(
        !root.join("--tmp-usecases--").exists(),
        "both lazy attempts create no session bucket"
    );
}

#[test]
fn empty_lazy_session_quiesces_without_materializing_and_retires_writer() {
    let (service, root) = service("empty-close");
    let baseline = crate::session::write_behind::live_writers_for_test();
    service.new_session(&project()).expect("new");
    service.quiesce_active().expect("empty close");
    wait_for_writer_baseline(baseline);
    assert!(
        !root.join("--tmp-usecases--").exists(),
        "an empty lazy session creates no session bucket"
    );
}

/// 不变量（2026-08-19 CI 失败根因）：忘记显式 quiesce/close 而
/// drop 的活动会话也必须退役 writer——JoinHandle 的 drop 是分离，
/// worker 在 condvar 上永生；泄漏的 writer 把并行套件里任何
/// `wait_for_writer_baseline` 的窗口顶红（慢速 CI 必现）。
/// `SessionCoordinator` 的 Drop 安全网保证这一点。
///
/// 观察手段必须是**每实例存活探针**而非全局 writer 计数：全局计
/// 数在并行套件里随别家测试的 writer 生灭抖动（第二版测试的
/// spawn 断言因此把"计数恰好持平"误报成失败，CI 二连红）。
/// pre-fix（无 Drop 安全网）：worker 永生，2s 轮询后断言失败。
#[test]
fn dropping_the_active_session_retires_its_writer() {
    use std::sync::atomic::Ordering;
    let (service, root) = service("drop-retires");
    let summary = service.new_session(&project()).expect("new");
    let _ = summary;
    run_turn(&service, "leave the writer holding a pending batch").expect("run");
    let coordinator = {
        let guard = service.active.lock().expect("active");
        guard.as_ref().expect("active session").coordinator.clone()
    };
    let alive = coordinator.writer_alive_handle_for_test();
    assert!(
        alive.load(Ordering::SeqCst),
        "the active session spawned a writer"
    );
    // 故意不 quiesce：drop 路径自己必须收拾线程（并尽力 flush）。
    drop(coordinator);
    drop(service);
    // close 同步 join，drop 返回即已退出——轮询只是 CI 磁盘 hiccup
    // 的余量（fsync 慢过 5s 才会吃满）。
    let mut retired = false;
    for _ in 0..500 {
        if !alive.load(Ordering::SeqCst) {
            retired = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        retired,
        "dropping the active session must retire its writer (still alive after 5s)"
    );
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn quiesce_fold_error_still_joins_the_writer() {
    let (service, root) = service("error-close");
    let baseline = crate::session::write_behind::live_writers_for_test();
    let summary = service.new_session(&project()).expect("new");
    let direct = {
        let guard = service.active.lock().expect("active");
        guard
            .as_ref()
            .expect("active session")
            .coordinator
            .journal()
    };
    direct
        .append(crate::session::run_journal::NewSessionEvent::new(
            "turn/start",
            payloads::turn_start(1),
        ))
        .expect("append");
    direct.flush().expect("commit outside folding journal");
    let log = root
        .join("--tmp-usecases--")
        .join(summary.id.as_str())
        .join("session.v2.jsonl.zstd");
    std::fs::write(&log, b"corrupt").expect("corrupt after commit");
    assert!(service.quiesce_active().is_err());
    wait_for_writer_baseline(baseline);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn new_resume_and_list_round_trip_with_checkpoints() {
    let (service, root) = service("roundtrip");
    let project = project();
    let summary = service.new_session(&project).expect("new");
    run_turn(&service, "hello world").expect("run");
    service.sync_active().expect("sync");
    let key = SessionKey {
        project: project.clone(),
        id: summary.id.clone(),
    };

    // Detach (flush + checkpoint), then resume: the view carries the
    // conversation, and the seed marker landed exactly once.
    service.quiesce_active().expect("detach");
    let view = service.resume(&key).expect("resume");
    assert!(
        view.transcript
            .iter()
            .any(|line| line.kind == "user" && line.text == "hello world")
    );
    assert_eq!(view.turns, 1);
    assert!(!view.model_items.is_empty());

    // Second resume does not grow the log with another seed marker.
    service.quiesce_active().expect("detach 2");
    service.resume(&key).expect("resume 2");
    service.quiesce_active().expect("detach 3");
    let loaded = service.backend.load(&key, false).expect("load");
    let seed_markers = loaded
        .events
        .iter()
        .filter(|event| event.event_type == "session/end-seed")
        .count();
    assert_eq!(seed_markers, 1, "untouched reopens do not grow the log");

    // List reads the checkpoint: title/stats present without decoding
    // log bodies.
    let summaries = service.list_sessions(&project).expect("list");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].turns, 1);
    assert!(
        summaries[0].last_activity_ms >= summaries[0].created_at_ms
            || summaries[0].last_activity_ms == summaries[0].created_at_ms
    );

    crate::test_support::cleanup_tree(&root);
}

#[test]
fn staging_is_read_only_and_failed_resume_keeps_the_active_session() {
    let (service, root) = service("stage-read-only");
    let project = project();
    let first = service.new_session(&project).expect("first");
    run_turn(&service, "keep me active").expect("run first");

    let bad_key = SessionKey {
        project: project.clone(),
        id: SessionId::new("unsupported-target"),
    };
    let bad_header = SessionHeader::new(bad_key.id.clone(), bad_key.project.header_cwd.clone(), 7);
    let prepared = service
        .backend
        .create(bad_key.clone(), bad_header)
        .expect("register target");
    service
        .backend
        .append_batch(
            prepared,
            0,
            &[SessionEvent::new(
                "future/required",
                0,
                8,
                serde_json::json!({"opaque": true}),
            )],
        )
        .expect("materialize unsupported target");

    assert!(service.resume(&bad_key).is_err());
    assert_eq!(
        service.active_id().as_ref(),
        Some(&first.id),
        "target admission must finish before the current session is quiesced"
    );

    let first_key = SessionKey {
        project,
        id: first.id,
    };
    service.quiesce_active().expect("detach first");
    service.checkpoints.drop(&first_key);
    let staged = service.stage_resume(&first_key).expect("stage first");
    assert!(
        service.checkpoints.load(&first_key).is_none(),
        "staging must not publish a derived checkpoint before workspace CAS"
    );
    let armed = service.arm_session(staged).expect("arm first");
    service.install_armed(armed);
    service.quiesce_active().expect("cleanup session");
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn title_cas_rejects_stale_and_accepts_force() {
    let (service, root) = service("title");
    let project = project();
    let summary = service.new_session(&project).expect("new");
    let session = summary.id.clone();
    run_turn(&service, "fix the login bug").expect("run");
    service.sync_active().expect("sync");

    // A late automatic rename against NoTitle loses to the manual one.
    assert!(
        service
            .set_title(
                &session,
                SetTitleExpectation::NoTitle,
                "manual name",
                TitleSource::User
            )
            .expect("manual")
    );
    assert!(
        !service
            .set_title(
                &session,
                SetTitleExpectation::NoTitle,
                "late auto",
                TitleSource::Provider {
                    provider: "prov",
                    model: "mdl",
                },
            )
            .expect("cas check"),
        "NoTitle no longer matches"
    );
    // Force always wins.
    assert!(
        service
            .set_title(
                &session,
                SetTitleExpectation::Force,
                "forced",
                TitleSource::User
            )
            .expect("force")
    );
    service.quiesce_active().expect("detach");

    let summaries = service.list_sessions(&project).expect("list");
    assert_eq!(summaries[0].title.as_deref(), Some("forced"));
    crate::test_support::cleanup_tree(&root);
}

/// 第四轮复审 F-A：迟到的自动命名 job 绑定原会话——切换后不得把
/// 标题写进当前活动会话（修复前：期望值来自旧会话、写入作用于新
/// 会话，两个都 NoTitle 时标题落错日志）。
#[test]
fn stale_title_jobs_never_write_into_the_switched_to_session() {
    let (service, root) = service("title-race");
    let project = project();
    let first = service.new_session(&project).expect("first");
    run_turn(&service, "first session prompt").expect("run");
    service.sync_active().expect("sync");

    // Switch away (quiesce + new active session with no title).
    service.quiesce_active().expect("quiesce first");
    let second = service.new_session(&project).expect("second");
    assert_ne!(first.id, second.id);

    // The stale job for the first session: silent no-op, never a write.
    assert!(
        !service
            .set_title(
                &first.id,
                SetTitleExpectation::NoTitle,
                "late title",
                TitleSource::Provider {
                    provider: "prov",
                    model: "mdl",
                },
            )
            .expect("stale job is a no-op, not an error")
    );
    // The active session stays untitled; a bound write still works.
    let (title, _) = service.title_state();
    assert_eq!(title, None, "no title leaked into the second session");
    assert!(
        service
            .set_title(
                &second.id,
                SetTitleExpectation::NoTitle,
                "bound title",
                TitleSource::User
            )
            .expect("bound write")
    );
    service.quiesce_active().expect("detach");

    let summaries = service.list_sessions(&project).expect("list");
    let by_id = |id: &crate::session::id::SessionId| {
        summaries
            .iter()
            .find(|summary| summary.id == *id)
            .expect("summary")
    };
    assert_eq!(by_id(&second.id).title.as_deref(), Some("bound title"));
    // The first session's summary shows its fallback title (derived
    // from the first user message) — the invariant is that NO explicit
    // `session/title` event was written to its log.
    let key = SessionKey {
        project: project.clone(),
        id: first.id.clone(),
    };
    let loaded = service.backend.load(&key, false).expect("load first");
    assert!(
        !loaded
            .events
            .iter()
            .any(|event| event.event_type == "session/title"),
        "the stale job never titled the first session either"
    );
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn deleting_checkpoints_changes_nothing_but_replay() {
    let (service, root) = service("drop-cache");
    let project = project();
    let summary = service.new_session(&project).expect("new");
    run_turn(&service, "one").expect("run");
    run_turn(&service, "two").expect("run 2");
    service.sync_active().expect("sync");
    let key = SessionKey {
        project: project.clone(),
        id: summary.id.clone(),
    };
    service.quiesce_active().expect("detach");
    let with_cache = service.resume(&key).expect("resume");
    service.quiesce_active().expect("detach");

    // Delete every checkpoint: cold resume must produce the same view.
    service.checkpoints.drop(&key);
    let without_cache = service.resume(&key).expect("resume from log");
    assert_eq!(without_cache.transcript, with_cache.transcript);
    assert_eq!(without_cache.turns, with_cache.turns);
    assert_eq!(without_cache.title, with_cache.title);
    service.quiesce_active().expect("cleanup detach");
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn recent_inputs_come_from_the_transcript() {
    let (service, root) = service("inputs");
    let project = project();
    service.new_session(&project).expect("new");
    run_turn(&service, "first question").expect("run");
    run_turn(&service, "second question").expect("run 2");
    let inputs = service.recent_inputs(10).expect("inputs");
    assert_eq!(inputs, vec!["first question", "second question"]);
    let limited = service.recent_inputs(1).expect("limited");
    assert_eq!(limited, vec!["second question"]);
    service.quiesce_active().expect("detach");
    crate::test_support::cleanup_tree(&root);
}

/// 并行测试会同时持有各自的 writer：断言用"回到基线"的轮询形式。
/// 预算 30s（正常路径立即返回）：CI 慢机上兄弟测试的 writer 可能
/// 存活数秒；真泄漏时会留下约 100 个 writer 永不退休，照样超时。
fn wait_for_writer_baseline(baseline: usize) {
    for _ in 0..1_200 {
        if crate::session::write_behind::live_writers_for_test() <= baseline {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!(
        "writer threads did not retire (baseline {baseline}, now {})",
        crate::session::write_behind::live_writers_for_test()
    );
}

#[test]
fn a_hundred_session_switches_retire_every_writer_thread() {
    let (service, root) = service("switches");
    let project = project();
    let baseline = crate::session::write_behind::live_writers_for_test();
    let mut last_id = None;
    for round in 0..100 {
        let summary = service.new_session(&project).expect("new");
        run_turn(&service, &format!("round {round}")).expect("run");
        // The quiesce is the detach boundary: it must join the writer,
        // not leak it (审计 P1-07 的线程泄漏反例)。
        service
            .quiesce_active()
            .unwrap_or_else(|error| panic!("quiesce failed: {error}"));
        last_id = Some(summary.id);
    }
    wait_for_writer_baseline(baseline);
    // And the last session still resumes with its content intact.
    let key = SessionKey {
        project: project.clone(),
        id: last_id.expect("id"),
    };
    let view = service.resume(&key).expect("resume");
    assert!(
        view.transcript
            .iter()
            .any(|line| line.kind == "user" && line.text == "round 99")
    );
    service.quiesce_active().expect("cleanup");
    crate::test_support::cleanup_tree(&root);
}

/// 复审第二轮：install 时 arm writer 会做 torn-tail 修复并追加合成
/// closer；追赶折叠必须把这些事件折进 staged 投影，否则中断 turn 不
/// 计数、下一轮 turn 号可能与中断轮撞号。
/// 第三轮复审：并发生产者在 inner.flush 与 pending 取出之间 append 的
/// 事件尚未提交——折叠必须止步于 committed 游标，否则一次 NotCommitted
/// 回滚就会让投影越过 seq 空洞领先于磁盘。
struct SteppedInnerJournal {
    next_seq: std::sync::atomic::AtomicU64,
    committed: std::sync::atomic::AtomicU64,
}

impl RunJournal for SteppedInnerJournal {
    fn append_atomic(
        &self,
        events: &[crate::session::run_journal::NewSessionEvent],
    ) -> Result<crate::session::run_journal::SeqRange, String> {
        use std::sync::atomic::Ordering;
        let start = self
            .next_seq
            .fetch_add(events.len() as u64, Ordering::SeqCst);
        Ok(crate::session::run_journal::SeqRange {
            start,
            end_inclusive: start + events.len() as u64 - 1,
        })
    }
    fn flush(&self) -> Result<(), String> {
        Ok(())
    }
    fn committed_seq(&self) -> Option<u64> {
        Some(self.committed.load(std::sync::atomic::Ordering::SeqCst))
    }
}

#[test]
fn direct_folds_never_pass_the_committed_cursor() {
    let root = std::env::temp_dir().join(format!(
        "clat-foldcursor-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let backend = Arc::new(JsonlBackend::new(
        &root,
        crate::session::persistence::JsonlCompression::Zstd,
        true,
    ));
    let key = SessionKey {
        project: project(),
        id: SessionId::new("cursor"),
    };
    let inner = Arc::new(SteppedInnerJournal {
        next_seq: std::sync::atomic::AtomicU64::new(0),
        committed: std::sync::atomic::AtomicU64::new(0),
    });
    let projections = Arc::new(Mutex::new(ProjectionRegistry::clat()));
    let journal = ProjectionFoldJournal::new(
        Arc::clone(&inner) as Arc<dyn RunJournal>,
        Arc::clone(&projections),
        Arc::clone(&backend),
        key,
    );
    let event = |turn: u64| {
        crate::session::run_journal::NewSessionEvent::new("turn/start", payloads::turn_start(turn))
    };
    let floor = || projections.lock().expect("projections").live_floor();

    // Two events appended, both committed: both fold.
    journal
        .append_atomic(&[event(1), event(2)])
        .expect("append 1");
    inner
        .committed
        .store(1, std::sync::atomic::Ordering::SeqCst);
    journal.flush().expect("flush 1");
    assert_eq!(floor(), 2, "committed events fold");

    // Two more appended (seqs 2,3); the commit cursor only reaches 2 —
    // seq 3 must stay queued, not folded.
    journal
        .append_atomic(&[event(3), event(4)])
        .expect("append 2");
    inner
        .committed
        .store(2, std::sync::atomic::Ordering::SeqCst);
    journal.flush().expect("flush 2");
    assert_eq!(floor(), 3, "the uncommitted tail stays queued");

    // The commit lands: the next flush picks it up.
    inner
        .committed
        .store(3, std::sync::atomic::Ordering::SeqCst);
    journal.flush().expect("flush 3");
    assert_eq!(floor(), 4, "the committed event folds on the next flush");
    std::fs::remove_dir_all(root).ok();
}

struct AppendFlushOverlapJournal {
    append_active: std::sync::atomic::AtomicBool,
    overlap: std::sync::atomic::AtomicBool,
}

impl RunJournal for AppendFlushOverlapJournal {
    fn append_atomic(
        &self,
        events: &[crate::session::run_journal::NewSessionEvent],
    ) -> Result<crate::session::run_journal::SeqRange, String> {
        self.append_active
            .store(true, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(40));
        self.append_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(crate::session::run_journal::SeqRange {
            start: 0,
            end_inclusive: events.len() as u64 - 1,
        })
    }

    fn flush(&self) -> Result<(), String> {
        if self.append_active.load(std::sync::atomic::Ordering::SeqCst) {
            self.overlap
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }

    fn committed_seq(&self) -> Option<u64> {
        Some(0)
    }
}

#[test]
fn projection_registration_is_atomic_against_concurrent_flush() {
    let root = std::env::temp_dir().join(format!(
        "clat-fold-lane-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let backend = Arc::new(JsonlBackend::new(
        &root,
        crate::session::persistence::JsonlCompression::Zstd,
        true,
    ));
    let inner = Arc::new(AppendFlushOverlapJournal {
        append_active: std::sync::atomic::AtomicBool::new(false),
        overlap: std::sync::atomic::AtomicBool::new(false),
    });
    let journal = Arc::new(ProjectionFoldJournal::new(
        Arc::clone(&inner) as Arc<dyn RunJournal>,
        Arc::new(Mutex::new(ProjectionRegistry::clat())),
        backend,
        SessionKey {
            project: project(),
            id: SessionId::new("fold-lane"),
        },
    ));
    let append_journal = Arc::clone(&journal);
    let append = std::thread::spawn(move || {
        append_journal.append_atomic(&[crate::session::run_journal::NewSessionEvent::new(
            "turn/start",
            payloads::turn_start(1),
        )])
    });
    while !inner
        .append_active
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        std::thread::yield_now();
    }
    journal.flush().expect("flush");
    append.join().unwrap().expect("append");
    assert!(
        !inner.overlap.load(std::sync::atomic::Ordering::SeqCst),
        "flush cannot pass queue admission before pending registration"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn torn_tail_resume_counts_the_interrupted_turn() {
    let (service, root) = service("tornresume");
    let project = project();
    let summary = service.new_session(&project).expect("new");
    run_turn(&service, "first complete turn").expect("run");
    // Append an open second turn, then tear the file mid-frame.
    let journal = service.journal().expect("journal");
    journal
        .append_atomic(&[
            crate::session::run_journal::NewSessionEvent::new(
                "turn/start",
                payloads::turn_start(2),
            ),
            crate::session::run_journal::NewSessionEvent::new(
                "user/message",
                payloads::user_message("second"),
            )
            .append(Vec::new()),
        ])
        .expect("open turn");
    journal.flush().expect("flush");
    service.quiesce_active().expect("detach");
    let key = SessionKey {
        project: project.clone(),
        id: summary.id.clone(),
    };
    let log = root
        .join("--tmp-usecases--")
        .join(summary.id.as_str())
        .join("session.v2.jsonl.zstd");
    let bytes = std::fs::read(&log).expect("read");
    std::fs::write(&log, &bytes[..bytes.len() - 3]).expect("tear");

    let view = service.resume(&key).expect("resume");
    assert_eq!(view.turns, 2, "the interrupted turn is closed and counted");
    assert_eq!(
        service.active_turns().expect("turns"),
        2,
        "the next run's turn number must not collide with turn 2"
    );
    service.quiesce_active().expect("cleanup");
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn staging_a_corrupt_target_fails_without_leaking_a_writer() {
    let (service, root) = service("corrupt");
    let project = project();
    let summary = service.new_session(&project).expect("new");
    run_turn(&service, "hello").expect("run");
    service.quiesce_active().expect("detach");
    let key = SessionKey {
        project: project.clone(),
        id: summary.id.clone(),
    };
    // Corrupt the log *semantically*: append a replace that cites
    // nonexistent surface nodes. prepare/admission pass (the bytes and
    // payloads are well-formed); cold restore fails and the read-only
    // stage path must leave no active session or writer behind.
    let loaded = service.backend.load(&key, false).expect("read len");
    let seq = loaded.events.len() as u64;
    let mut bad = crate::session::event::SessionEvent::new(
        "user/message",
        seq,
        9_999,
        crate::session::event::payloads::user_message("[bogus summary]"),
    );
    bad.surface_op = Some(crate::session::event::SurfaceOp::Replace { start: 99, end: 99 });
    bad.source_event_seqs = Some(vec![99]);
    let frame = crate::session::jsonl::append_batch_bytes(
        &[bad],
        crate::session::persistence::JsonlCompression::Zstd,
        true,
    )
    .expect("encode");
    let log = root
        .join("--tmp-usecases--")
        .join(summary.id.as_str())
        .join("session.v2.jsonl.zstd");
    let mut bytes = std::fs::read(&log).expect("read");
    bytes.extend_from_slice(&frame);
    std::fs::write(&log, &bytes).expect("append");
    let baseline = crate::session::write_behind::live_writers_for_test();
    let staged = service.stage_resume(&key).expect("bounded header stage");
    assert!(service.arm_session(staged).is_err());
    assert!(
        service.active_id().is_none(),
        "a failed stage must not install anything"
    );
    wait_for_writer_baseline(baseline);
    crate::test_support::cleanup_tree(&root);
}

/// INV-C1/C2：usage 折叠按 journal `source {provider, model}` 路由
/// 分桶——Cache 口径归属当前模型路由（切换不混合不清零），session
/// 口径仍是全会话累计（TUI-L04 不变），last_request 取最近一次。
/// 修复前该测试无处安放：UsageStats 没有路由桶，跨模型的缓存命中
/// 会混进同一个百分比（用户报告：GLM→DeepSeek 切换后 Cache 残留）。
#[test]
fn usage_fold_buckets_by_model_route() {
    let (service, root) = service("usage-routes");
    let summary = service.new_session(&project()).expect("session");
    let key = SessionKey {
        id: summary.id.clone(),
        project: project(),
    };
    let glm_usage = crate::model::Usage {
        input_tokens: 1000,
        cached_input_tokens: Some(800),
        ..crate::model::Usage::default()
    };
    let ds_usage = crate::model::Usage {
        input_tokens: 200,
        cached_input_tokens: Some(0),
        ..crate::model::Usage::default()
    };
    let journal = service.journal().expect("journal");
    journal
        .append_atomic(&[
            crate::session::run_journal::NewSessionEvent::new(
                "assistant/message",
                payloads::assistant_message(
                    1,
                    1,
                    vec![payloads::text_block("glm answer")],
                    "OpenAI Compatible",
                    "glm-5.3",
                    Some(&glm_usage),
                ),
            )
            .append(Vec::new()),
            crate::session::run_journal::NewSessionEvent::new(
                "assistant/message",
                payloads::assistant_message(
                    2,
                    2,
                    vec![payloads::text_block("deepseek answer")],
                    "OpenAI Compatible",
                    "deepseek-v4-flash",
                    Some(&ds_usage),
                ),
            )
            .append(Vec::new()),
        ])
        .expect("append");
    journal.flush().expect("flush");

    let (_replay, usage) = service.replay_with_usage(&key).expect("replay");
    let glm = usage
        .routes
        .get("OpenAI Compatible/glm-5.3")
        .expect("glm bucket survives the switch");
    assert_eq!(glm.input_tokens, 1000);
    assert_eq!(glm.cached_input_tokens, Some(800));
    let ds = usage
        .routes
        .get("OpenAI Compatible/deepseek-v4-flash")
        .expect("deepseek bucket");
    assert_eq!(ds.input_tokens, 200);
    assert_eq!(ds.cached_input_tokens, Some(0));
    // session 口径跨路由累计；最近一次是 deepseek。
    assert_eq!(usage.session.input_tokens, 1200);
    assert_eq!(usage.session.cached_input_tokens, Some(800));
    assert_eq!(
        usage
            .last_request
            .as_ref()
            .and_then(|u| u.cached_input_tokens),
        Some(0)
    );
    crate::test_support::cleanup_tree(&root);
}
