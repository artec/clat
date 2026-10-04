"""Isolated core-network semantic deletion checks; never change shared sources."""
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[2]
OUT = pathlib.Path(sys.argv[1])
assert OUT.is_absolute()
OUT.mkdir()  # Refuse overwrite.
SNAPSHOT = '''let generation = self.network_generation.load(Ordering::Acquire);
        let services = self
            .project_services
            .read()
            .ok()
            .and_then(|services| services.clone())
            .ok_or(NetworkError::Unavailable)?;
        #[cfg(test)]
        tests::after_services_snapshot();'''
CASES = [
    ('late-plan-allow', 'src/plugin_host/network.rs', 'self.check()?;\n        if decision', '// deleted post-approval check\n        if decision', 'core_network_access_revision'),
    ('access-lease-revision', 'src/plugin_host/network.rs', 'self.services.permissions.revision() != self.factory_revision', 'false', 'core_network_access_revision'),
    ('access-factory-revision', 'src/plugins/permission.rs', 'self.access.as_ref().map_or(0, |access| access.revision())', '0', 'core_network_access_revision'),
    ('access-slot-revision', 'src/tool/access.rs', 'self.revision\n                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);', '// deleted access publication revision', 'core_network_access_revision'),
    ('child-deadline', 'src/plugin_host/network.rs', 'let deadline = deadline.min(self.deadline);', '// deleted tool deadline clamp', 'core_network_child_approval_deadline'),
    ('factory', 'src/plugin_host/network.rs', 'let policy = self\n            .services\n            .permissions\n            .create(self.approver.clone(), &self.cancel);', 'let policy: Box<dyn crate::PermissionPolicy> = Box::new(crate::AllowAll);', 'core_network_factory_preserves_plan_guard'),
    ('fixed-network', 'src/plugin_host/network.rs', 'effect: crate::ToolEffect::Network,', 'effect: crate::ToolEffect::Read,', 'core_network_factory_applies_modes'),
    ('run-epoch', 'src/plugin_host/network.rs', '!bridge.context_is_current(self.epoch, &self.cancel)', 'self.cancel.is_cancelled()', 'core_network_lease_rejects_clear'),
    ('parent-cancel', 'src/plugin_host/network.rs', '!bridge.context_is_current(self.epoch, &self.cancel)', 'bridge.installed_epoch() != Some(self.epoch)', 'core_network_lease_rejects_clear'),
    ('generation', 'src/plugin_host/network.rs', 'bridge.network_generation.load(Ordering::Acquire) != self.generation', 'false', 'core_network_lease_rejects_generation'),
    ('late-allow', 'src/plugin_host/network.rs', 'self.check()?;\n        if decision', '// deleted post-approval check\n        if decision', 'core_network_late_allow'),
    ('tool-cap', 'src/plugin_host/network.rs', 'Duration::from_secs(120)', 'Duration::from_secs(121)', 'core_network_lease_has_no_context'),
    ('services-refresh', 'src/plugin_host.rs', 'self.invalidate_network_context();\n            *services', '// deleted service generation invalidation\n            *services', 'core_network_lease_rejects_clear'),
    ('snapshot-order', 'src/plugin_host/network.rs', SNAPSHOT, SNAPSHOT.replace('let generation = self.network_generation.load(Ordering::Acquire);\n        ', '') + '\n        let generation = self.network_generation.load(Ordering::Acquire);', 'core_network_service_refresh_during_snapshot'),
    ('publication', 'src/application/trusted.rs', 'self.plugin_host.invalidate_network_context();', '// deleted network publication invalidation', 'core_network_application_publication'),
    ('executor-factory', 'src/plugin_host/network/approval.rs', 'let result = self.lease.approve(&self.request);', 'let result = Ok(());', 'core_network_executor_preserves_factory'),
    ('executor-isolation', 'src/plugin_host/network/approval.rs', 'operation.cancel = lease.cancel.child_with_deadline(lease.deadline);', 'operation.cancel = lease.cancel.clone();', 'core_network_executor_drop_cancels_only_operation'),
    ('executor-drop', 'src/plugin_host/network/approval.rs', 'fn drop(&mut self) {\n        self.lease.cancel.cancel();', 'fn drop(&mut self) {\n        // deleted operation cancellation', 'core_network_executor_drop_cancels_only_operation'),
    ('executor-consume-check', 'src/plugin_host/network/approval.rs', 'if let Err(error) = self.lease.check() {\n            self.lease.cancel.cancel();\n            return Some(Err(error));\n        }', '// deleted consumer revocation check', 'core_network_executor_revokes_ready_allow'),
    ('executor-terminal-cancel', 'src/plugin_host/network/approval.rs', 'self.lease.cancel.cancel();\n            return Some(Err(error));', '// deleted terminal cancellation\n            return Some(Err(error));', 'core_network_executor_drop_cancels_only_operation'),
    ('executor-single-consumption', 'src/plugin_host/network/approval.rs', 'std::mem::replace(&mut *state, State::Consumed)', 'std::mem::replace(&mut *state, State::Pending)', 'core_network_executor_preserves_factory'),
    ('executor-nonblocking', 'src/plugin_host/network/approval.rs', '.try_send(Work {', '.send(Work {', 'core_network_executor_is_nonblocking'),
]

if len(sys.argv) > 2:
    CASES = [case for case in CASES if case[0] == sys.argv[2]]
    assert CASES, "unknown deletion name"

def run(root, package, filter_name):
    return subprocess.run(['cargo', 'test', '-p', package, '--lib', '--locked', '--offline', '--target-dir', str(ROOT / 'target'), filter_name, '--', '--nocapture'], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)

report = []
originals = {file: (ROOT / file).read_text().replace('\r\n', '\n') for _, file, *_ in CASES}
for name, file, before, *_ in CASES:
    assert originals[file].count(before) == 1, name
with tempfile.TemporaryDirectory(prefix='clat-core-network-deletion-') as temp:
    clone = pathlib.Path(temp)
    ignore = shutil.ignore_patterns('target', 'node_modules', '.git', '.codegraph', 'output', '__pycache__')
    for name in ['src', 'crates', 'tests', 'plugins', 'sdk', 'wit', 'web', 'market', 'scripts', '.github']:
        shutil.copytree(ROOT / name, clone / name, ignore=ignore)
    for name in ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'LICENSE', 'README.md', 'README.zh.md', '.gitattributes']:
        shutil.copy2(ROOT / name, clone / name)
    (clone / 'release').mkdir()
    shutil.copy2(ROOT / 'release/minisign.pub', clone / 'release/minisign.pub')
    # Separate artifact identity; dependencies share the normal cache.
    # Distinguish every runner invocation as well as the canonical package:
    # interrupted runs in different temp roots must never share a Rust artifact.
    mutant = 'clat-core-network-deletion-' + uuid.uuid4().hex[:12]
    manifest = clone / 'crates/core/Cargo.toml'
    manifest.write_text(manifest.read_text().replace('name = "clat-core"', f'name = "{mutant}"'))
    root_manifest = clone / 'Cargo.toml'
    root_manifest.write_text(root_manifest.read_text().replace('clat-core = { path', f'clat-core = {{ package = "{mutant}", path').replace('[profile.test.package.clat-core]', f'[profile.test.package.{mutant}]'))
    lock = clone / 'Cargo.lock'
    lock.write_text(lock.read_text().replace('"clat-core"', f'"{mutant}"'))
    for name, file, before, after, filter_name in CASES:
        original = originals[file]
        assert original.count(before) == 1, name
        (clone / file).write_text(original.replace(before, after))
        result = run(clone, mutant, filter_name)
        (OUT / f'{name}.log').write_text(result.stdout)
        (clone / file).write_text(original)
        assert result.returncode == 101, (name, result.returncode)
        assert 'panicked at' in result.stdout and 'test result: FAILED.' in result.stdout, name
        assert 'could not compile' not in result.stdout and 'error[E' not in result.stdout, name
        report.append({'deletion': name, 'filter': filter_name, 'exit': result.returncode})
        print(json.dumps(report[-1]), flush=True)
    result = run(ROOT, 'clat-core', 'core_network')
    (OUT / 'canonical-green.log').write_text(result.stdout)
    assert result.returncode == 0 and '16 passed; 0 failed' in result.stdout
    for file, original in originals.items():
        assert (ROOT / file).read_text().replace('\r\n', '\n') == original
    (OUT / 'report.json').write_text(json.dumps({'deletions': report, 'canonicalPassed': 16, 'fullD1AttackMatrixComplete': False}, indent=2)+'\n')
