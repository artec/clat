use crate::application::host_tests::fixture;
#[test]
fn managed_skill_save_edit_delete() {
    let (root, mut host, first, _) = fixture("ms-h2");
    let app = host.attach(first).unwrap();
    let skill = "---\nname: custom-review\ndescription: Review code\n---\nUse the correct code review process.\n";
    let saved = app
        .lock()
        .unwrap()
        .skill_manage_save("user", "custom-review", "missing", skill)
        .unwrap();
    let rev = saved["revision"].as_str().unwrap();
    assert!(
        app.lock()
            .unwrap()
            .skill_manage_save("user", "custom-review", "missing", skill)
            .is_err()
    );
    assert_eq!(
        app.lock().unwrap().skill_manage_list().unwrap()["user"][0]["name"],
        "custom-review"
    );
    assert_eq!(
        app.lock()
            .unwrap()
            .skill_manage_get("user", "custom-review")
            .unwrap()["revision"],
        rev
    );
    app.lock()
        .unwrap()
        .skill_manage_remove("user", "custom-review", rev)
        .unwrap();
    drop(app);
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn managed_skill_new_run_catalog_and_project_shadowing() {
    let (root, mut host, first, second) = fixture("ms-h2-layered");
    let first_app = host.attach(first).unwrap();
    let second_app = host
        .authorize_and_attach(second, crate::ProjectAuthorization::grant())
        .unwrap();
    let user = "---\nname: audit-custom\ndescription: Shared review\n---\nShared instructions\n";
    first_app
        .lock()
        .unwrap()
        .skill_manage_save("user", "audit-custom", "missing", user)
        .unwrap();
    let project = "---\nname: audit-custom\ndescription: Project review\n---\nProject-specific instructions\n";
    first_app
        .lock()
        .unwrap()
        .skill_manage_save("project", "audit-custom", "missing", project)
        .unwrap();
    {
        let app = first_app.lock().unwrap();
        let snapshot = app.skills_snapshot().unwrap();
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|e| e.name == "audit-custom")
                .unwrap()
                .source
                .as_str(),
            "project"
        );
        let loaded = app
            .skills_service()
            .unwrap()
            .load(&snapshot, "audit-custom", None)
            .unwrap();
        assert_eq!(loaded["body"], "Project-specific instructions\n");
    }
    {
        let app = second_app.lock().unwrap();
        let snapshot = app.skills_snapshot().unwrap();
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|e| e.name == "audit-custom")
                .unwrap()
                .source
                .as_str(),
            "user"
        );
        let loaded = app
            .skills_service()
            .unwrap()
            .load(&snapshot, "audit-custom", None)
            .unwrap();
        assert_eq!(loaded["body"], "Shared instructions\n");
    }
    let rev = first_app
        .lock()
        .unwrap()
        .skill_manage_get("project", "audit-custom")
        .unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    first_app
        .lock()
        .unwrap()
        .skill_manage_remove("project", "audit-custom", &rev)
        .unwrap();
    assert_eq!(
        first_app
            .lock()
            .unwrap()
            .skills_overview()
            .unwrap()
            .entries
            .iter()
            .find(|e| e.name == "audit-custom")
            .unwrap()
            .source,
        "user"
    );
    drop(first_app);
    drop(second_app);
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[cfg(unix)]
#[test]
fn managed_skill_rejects_symlink_layers_before_opening_them() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!("clat-ms1-skill-layer-{}", uuid::Uuid::new_v4()));
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join("skills")).unwrap();
    let text = "---\nname: guard-review\ndescription: Review\n---\nCheck the source.\n";
    let error =
        crate::skills::SkillFiles::save(&root, true, "guard-review", "missing", text).unwrap_err();
    assert!(
        error.contains("Skill layer must be a real directory"),
        "{error}"
    );
    assert!(!outside.join("guard-review").exists());

    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    symlink(&outside, project.join(".clat")).unwrap();
    let error = crate::skills::SkillFiles::save(&project, false, "guard-review", "missing", text)
        .unwrap_err();
    assert!(
        error.contains("Skill layer must be a real directory"),
        "{error}"
    );
    assert!(!outside.join("skills").exists());
    crate::test_support::cleanup_tree(&root);
}

#[cfg(unix)]
#[test]
fn managed_skill_rejects_symlink_bundle_without_writing_outside_root() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!("clat-ms1-skill-bundle-{}", uuid::Uuid::new_v4()));
    let outside = root.join("outside");
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join("skills").join("guard-review")).unwrap();
    let text = "---\nname: guard-review\ndescription: Review\n---\nCheck the source.\n";
    let error =
        crate::skills::SkillFiles::save(&root, true, "guard-review", "missing", text).unwrap_err();
    assert!(
        error.contains("Skill bundle must be a real directory"),
        "{error}"
    );
    assert!(!outside.join("SKILL.md").exists());
    assert!(crate::skills::SkillFiles::read(&root, true, "guard-review").is_err());
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn managed_skill_rejects_traversal_name_before_creating_a_bundle() {
    let root = std::env::temp_dir().join(format!("clat-ms1-skill-path-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let text = "---\nname: guard-review\ndescription: Review\n---\nCheck the source.\n";
    assert!(
        crate::skills::SkillFiles::save(&root, true, "../guard-review", "missing", text).is_err()
    );
    assert!(crate::skills::SkillFiles::read(&root, true, "../guard-review").is_err());
    assert!(!root.join("skills").exists());
    crate::test_support::cleanup_tree(&root);
}
