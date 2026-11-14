"""Provisioning logic runs against temporary repos and fake OS commands, never this host."""
import os
import shlex
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class UbuntuProvisionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='ck ubuntu test ')
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.env = dict(os.environ, HOME=str(self.base), GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null')

    def shell(self, code, *, check=False):
        source = (ROOT / 'scripts/provision.sh').read_text()
        marker = '# Provisioning starts here'
        self.assertTrue(marker in source, 'Provision helpers must be sourceable without running OS setup')
        helpers = source.split(marker)[0]
        return subprocess.run(['bash', '-c', helpers + '\nset -e\n' + code], cwd=self.base, env=self.env, text=True, capture_output=True, check=check)

    def git(self, *args, cwd=None):
        return subprocess.run(['git', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', *args], cwd=cwd or self.base, env=self.env, check=True, text=True, capture_output=True).stdout.strip()

    def test_git_updates_and_preserves_local_edits(self):
        remote = self.base / 'remote.git'
        seed = self.base / 'seed'
        work = self.base / 'Desktop/cluster-kit'
        self.git('init', '--bare', str(remote))
        self.git('clone', str(remote), str(seed))
        (seed / 'version').write_text('one')
        self.git('add', '.', cwd=seed)
        self.git('commit', '-m', 'initial', cwd=seed)
        self.git('branch', '-M', 'main', cwd=seed)
        self.git('push', 'origin', 'main', cwd=seed)
        code = f'ck_git_sync "{work}" "{remote}" main'
        self.assertEqual(self.shell(code).returncode, 0)
        (seed / 'version').write_text('two')
        self.git('commit', '-am', 'update', cwd=seed)
        self.git('push', 'origin', 'main', cwd=seed)
        self.assertEqual(self.shell(code).returncode, 0)
        self.assertEqual((work / 'version').read_text(), 'two')
        (work / 'version').write_text('local edit')
        self.assertNotEqual(self.shell(code).returncode, 0)
        self.assertEqual((work / 'version').read_text(), 'local edit')
        self.assertTrue((work / '.git').is_dir())

    def test_divergent_repo_is_not_overwritten(self):
        self.test_git_updates_and_preserves_local_edits()
        work = self.base / 'Desktop/cluster-kit'
        seed = self.base / 'seed'
        self.git('commit', '-am', 'local', cwd=work)
        (seed / 'version').write_text('remote edit')
        self.git('commit', '-am', 'remote', cwd=seed)
        self.git('push', 'origin', 'main', cwd=seed)
        before = self.git('rev-parse', 'HEAD', cwd=work)
        result = self.shell(f'ck_git_sync "{work}" "{self.base / "remote.git"}" main')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.git('rev-parse', 'HEAD', cwd=work), before)

    def test_install_repairs_interrupted_and_broken_packages(self):
        result = self.shell('''
count=0
apt-get() { printf 'apt %s\\n' "$*" >> calls; case " $* " in *' install curl ca-certificates '*) count=$((count+1)); [ "$count" -gt 1 ] ;; *' --fix-broken install '*) touch repaired; return 0 ;; *) return 0 ;; esac; }
dpkg() { printf 'dpkg %s\\n' "$*" >> calls; [ -f repaired ]; }
ck_install_packages curl ca-certificates
cat calls
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = result.stdout.splitlines()
        self.assertTrue(any('dpkg --configure -a' in line for line in lines))
        self.assertTrue(any('--fix-broken install' in line for line in lines))
        self.assertTrue(any('--fix-missing install curl ca-certificates' in line for line in lines))

    def test_stop_services_before_replacing_processes(self):
        result = self.shell('''
systemctl() { printf 'systemctl %s\\n' "$*" >> calls; }
pkill() { printf 'pkill %s\\n' "$*" >> calls; }
pgrep() { return 1; }
ck_stop_existing
cat calls
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('stop ck-worker ck-head ck-agent', result.stdout)
        self.assertIn('pkill -TERM -x ck', result.stdout)
        self.assertLess(result.stdout.index('stop ck-worker ck-head'), result.stdout.index('pkill -TERM'))

    def test_banner_reports_identity_and_verified_ssh(self):
        result = self.shell('''
hostname() { case "${1:-}" in -I) echo '192.168.1.9 10.0.0.9' ;; *) echo lab-9 ;; esac; }
ss() { echo 'LISTEN 0 128 0.0.0.0:22'; }
ck_banner lab-9
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        for value in ('lab-9', '192.168.1.9', '10.0.0.9', 'SSH: listening', 'ssh lab-9@192.168.1.9'):
            self.assertIn(value, result.stdout)

    def test_firmware_bootstrap_uses_existing_downloaders_and_rejects_failures(self):
        code = (ROOT / 'hid/ck_provision/ubuntu_bootstrap.h').read_text().split('R"CK(')[1].split(')CK"')[0]
        commands = self.base / 'commands'
        commands.mkdir()
        for name in ('mktemp', 'rm'):
            target = subprocess.check_output(['which', name], text=True).strip()
            (commands / name).symlink_to(target)
        payload = '#!/bin/bash\nprintf "downloaded-script-ran\\n"\n'
        (commands / 'curl').write_text('#!/bin/bash\nprintf "curl\\n" >> "$HOME/calls"\n'
            'while [ "$#" -gt 0 ]; do if [ "$1" = -o ]; then shift; break; fi; shift; done\n'
            'printf %s ' + shlex.quote(payload) + ' > "$1"\n'
            '[ "$MODE" = curl ]\n')
        (commands / 'wget').write_text('#!/bin/bash\nprintf "wget\\n" >> "$HOME/calls"\n'
            'printf %s ' + shlex.quote(payload) + ' > "$2"\n'
            '[ "$MODE" = wget ]\n')
        for name in ('curl', 'wget'):
            (commands / name).chmod(0o755)
        for mode, expected_calls in (('curl', ['curl']), ('wget', ['curl', 'wget']), ('fail', ['curl', 'wget'])):
            with self.subTest(mode=mode):
                (self.base / 'calls').write_text('')
                result = subprocess.run(['/bin/bash', '-c', code, 'ck-bootstrap', 'https://example.invalid/provision.sh'],
                    env=dict(self.env, PATH=str(commands), MODE=mode), text=True, capture_output=True)
                self.assertEqual((self.base / 'calls').read_text().splitlines(), expected_calls)
                if mode == 'fail':
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn('downloaded-script-ran', result.stdout)
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn('downloaded-script-ran', result.stdout)

    def test_release_download_is_verified_before_replacing_worker(self):
        import hashlib
        import json
        commit = 'a' * 40
        payload = b'#!/bin/sh\necho "usage: ck head|worker" >&2\nexit 1\n'
        (self.base / 'release-bin').write_bytes(payload)
        (self.base / 'releases.json').write_text(json.dumps([{'tag_name': 'linux-' + commit, 'draft': False, 'prerelease': False}]))
        manifest = {'commit': commit, 'target': 'x86_64-unknown-linux-musl', 'sha256': hashlib.sha256(payload).hexdigest()}
        (self.base / 'manifest.json').write_text(json.dumps(manifest))
        code = r'''
uname() { echo x86_64; }
ck_download() { case "$1" in *'releases?per_page='*) cp releases.json "$2" ;; *manifest.json) cp manifest.json "$2" ;; *) cp release-bin "$2" ;; esac; }
ck_stop_existing() { touch stopped; }
CK_REPLACE=1
ck_install_release TheDivyanshShukla/cluster-kit installed/ck installed/stamp
'''
        result = self.shell(code)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.base / 'installed/ck').read_bytes(), payload)
        self.assertIn(commit, (self.base / 'installed/stamp').read_text())
        (self.base / 'stopped').unlink()
        (self.base / 'release-bin').write_bytes(b'bad download')
        result = self.shell(code)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.base / 'stopped').exists())
        self.assertEqual((self.base / 'installed/ck').read_bytes(), payload)

    def test_apt_unlock_refuses_to_remove_files_while_owned(self):
        result = self.shell(r'''
fuser() { return 0; }
sleep() { :; }
rm() { echo unsafe-delete; }
CK_APT_LOCK_WAIT=0
ck_unlock_apt
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('unsafe-delete', result.stdout)
        self.assertIn('package manager is still running', result.stderr)

    def test_flash_selects_ubuntu_and_branch_without_reset(self):
        import shutil
        hid = self.base / 'hid'
        (hid / 'ck_provision').mkdir(parents=True)
        shutil.copyfile(ROOT / 'hid/flash.sh', hid / 'flash.sh')
        (hid / 'ck_provision/config.h').write_text('// test fixture')
        commands = self.base / 'bin'
        commands.mkdir()
        (commands / 'esptool').write_text("""#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$HOME/esptool-calls"
case "$*" in *--help*) echo write-flash ;; *chip-id*) echo ESP32-S3 ;; esac
""")
        (commands / 'arduino-cli').write_text("""#!/usr/bin/env bash
if [ "$1" = core ]; then echo esp32:esp32; exit; fi
printf '%s\\n' "$@" > "$HOME/compile-args"
while [ "$#" -gt 0 ]; do
  if [ "$1" = --output-dir ]; then shift; mkdir -p "$1"; printf image > "$1/ck_provision.ino.merged.bin"; fi
  shift
done
""")
        for item in commands.iterdir(): item.chmod(0o755)
        env = dict(self.env, PATH=str(commands) + ':' + os.environ['PATH'], CK_PROVISION_BRANCH='feature/general-purpose')
        result = subprocess.run(['bash', str(hid / 'flash.sh'), '--os', 'ubuntu', '/dev/fake'], env=env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        args = (self.base / 'compile-args').read_text().splitlines()
        flags = args[args.index('--build-property') + 1]
        self.assertIn('-DCK_TARGET_OS=1', flags)
        self.assertIn('feature/general-purpose', flags)
        calls = (self.base / 'esptool-calls').read_text().splitlines()
        self.assertIn('--after no-reset write-flash', '\n'.join(calls))
        for call in calls:
            if 'chip-id' in call or 'write-flash' in call:
                self.assertIn('--after no-reset', call)
        bad = subprocess.run(['bash', str(hid / 'flash.sh'), '--os', 'invalid'], env=env, text=True, capture_output=True)
        self.assertNotEqual(bad.returncode, 0)

    def test_copied_folder_migration_preserves_data_and_backup(self):
        self.test_git_updates_and_preserves_local_edits()
        import shutil
        work = self.base / 'Desktop/cluster-kit'
        shutil.rmtree(work / '.git')
        (work / 'ck-data').mkdir()
        (work / 'ck-data/test.db').write_text('local state')
        (work / 'rust/target/release').mkdir(parents=True)
        (work / 'rust/target/release/.ck-built').write_text('copied stale build stamp')
        result = self.shell(f'ck_git_sync "{work}" "{self.base / "remote.git"}" main')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((work / 'ck-data/test.db').read_text(), 'local state')
        self.assertFalse((work / 'rust/target/release/.ck-built').exists())
        backups = list(work.parent.glob('cluster-kit.before-git.*/cluster-kit/version'))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_text(), 'local edit')

    def test_generated_firmware_command_quotes_configuration_and_has_no_newlines(self):
        sketch = (ROOT / 'hid/ck_provision/ck_provision.ino').read_text()
        quote = sketch[sketch.index('String shellQuote('):sketch.index('void setup()')]
        begin = sketch.index('    String cmd = String(" printf')
        command = sketch[begin:sketch.index('    typeText(', begin)]
        cpp = self.base / 'command.cpp'
        cpp.write_text('#include <string>\n#include <iostream>\nusing String = std::string;\n'
                       '#define PASS "fixture apostrophe\' password"\n#define ADMIN_KEY ""\n'
                       '#define HEAD_ENV ""\n#define CK_PROVISION_BRANCH "feature/general-purpose"\n'
                       'const char *URL_SH = "https://example.invalid/provision.sh";\n'
                       + (ROOT / 'hid/ck_provision/ubuntu_bootstrap.h').read_text()
                       + '\n' + quote + '\nint main() {\n' + command + '\nstd::cout << cmd;\n}\n')
        binary = self.base / 'command'
        compiled = subprocess.run(['c++', str(cpp), '-o', str(binary)], capture_output=True, text=True)
        self.assertEqual(compiled.returncode, 0, compiled.stderr)
        generated = subprocess.run([str(binary)], capture_output=True, text=True, check=True).stdout
        self.assertNotIn('\n', generated)
        self.assertLess(len(generated), 1000)
        self.assertNotIn('install_curl()', generated)
        self.assertIn('/usr/bin/env PATH=/usr/sbin:/usr/bin:/sbin:/bin', generated)
        self.assertIn('/bin/bash -c', generated)
        self.assertEqual(generated.count('sudo '), 1)
        self.assertNotIn(' -v &&', generated)
        self.assertIn("printf '%s\\n'", generated)
        self.assertIn('CK_FOLLOW=1', generated)
        parsed = subprocess.run(['bash', '-n'], input=generated, capture_output=True, text=True)
        self.assertEqual(parsed.returncode, 0, parsed.stderr)

    def test_clean_main_moves_to_deployed_branch_but_local_commits_are_preserved(self):
        self.test_git_updates_and_preserves_local_edits()
        work = self.base / 'Desktop/cluster-kit'
        seed = self.base / 'seed'
        self.git('restore', 'version', cwd=work)
        self.git('checkout', '-b', 'feature/ubuntu', cwd=seed)
        (seed / 'version').write_text('ubuntu branch')
        self.git('commit', '-am', 'ubuntu', cwd=seed)
        self.git('push', 'origin', 'feature/ubuntu', cwd=seed)
        result = self.shell(f'ck_git_sync "{work}" "{self.base / "remote.git"}" feature/ubuntu')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git('branch', '--show-current', cwd=work), 'feature/ubuntu')
        self.assertEqual((work / 'version').read_text(), 'ubuntu branch')
        self.git('checkout', 'main', cwd=work)
        (work / 'version').write_text('local main commit')
        self.git('commit', '-am', 'local main', cwd=work)
        before = self.git('rev-parse', 'HEAD', cwd=work)
        result = self.shell(f'ck_git_sync "{work}" "{self.base / "remote.git"}" feature/ubuntu')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.git('rev-parse', 'HEAD', cwd=work), before)

    def test_provision_git_wrapper_passes_paths_as_separate_arguments(self):
        self.test_git_updates_and_preserves_local_edits()
        work = self.base / 'Desktop/cluster-kit'
        self.git('restore', 'version', cwd=work)
        source = (ROOT / 'scripts/provision.sh').read_text()
        start = source.index('\ngit_sync() {')
        wrapper = source[start:source.index('\nif [ $ONLINE', start)]
        result = self.shell('as_user() { "$@"; };\n' + f'DIR="{work}"; REPO="{self.base / "remote.git"}"; BRANCH=main;\n' + wrapper + '\ngit_sync')
        self.assertEqual(result.returncode, 0, result.stderr)


    def test_typing_preserves_repeated_keys_without_extra_delay(self):
        sketch = (ROOT / 'hid/ck_provision/ck_provision.ino').read_text()
        function = sketch[sketch.index('void typeText('):sketch.index('// Quote configuration')]
        cpp = self.base / 'typing.cpp'
        cpp.write_text('#include <string>\n#include <cassert>\nusing String = std::string;\n'
                       'unsigned waited = 0; void delay(unsigned ms) { waited += ms; }\n'
                       'void led(int, int, int) {}\n'
                       'struct { std::string typed; char held = 0;\n'
                       'void press(char c) { assert(!held); held = c; typed += c; }\n'
                       'void releaseAll() { held = 0; } } Keyboard;\n'
                       + function + '\nint main() { typeText("aaAA!!11");\n'
                       'assert(Keyboard.typed == "aaAA!!11"); assert(!Keyboard.held);\n'
                       'assert(waited == 0); }\n')
        binary = self.base / 'typing'
        subprocess.run(['c++', str(cpp), '-o', str(binary)], check=True, capture_output=True)
        result = subprocess.run([str(binary)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)


    def test_paced_typing_allows_host_to_observe_press_and_release(self):
        sketch = (ROOT / 'hid/ck_provision/ck_provision.ino').read_text()
        function = sketch[sketch.index('void typeText('):sketch.index('// Quote configuration')]
        cpp = self.base / 'paced-typing.cpp'
        cpp.write_text(r'''#include <string>
#include <cassert>
using String = std::string;
unsigned clock_ms = 0;
void delay(unsigned ms) { clock_ms += ms; }
void led(int, int, int) {}
struct {
    std::string typed;
    char held = 0;
    unsigned changed = 0;
    void press(char c) { assert(!held); assert(clock_ms - changed >= 8); held = c; changed = clock_ms; }
    void releaseAll() { assert(clock_ms - changed >= 8); typed += held; held = 0; changed = clock_ms; }
} Keyboard;
''' + function + r'''
int main() {
    clock_ms = 8;
    typeText("aaAA!!11", 8);
    assert(Keyboard.typed == "aaAA!!11");
    assert(!Keyboard.held);
    assert(clock_ms - Keyboard.changed >= 8);
}
''')
        binary = self.base / 'paced-typing'
        built = subprocess.run(['c++', str(cpp), '-o', str(binary)], capture_output=True, text=True)
        self.assertEqual(built.returncode, 0, built.stderr)
        result = subprocess.run([str(binary)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_checked_payload_runs_only_after_sha256_matches(self):
        import base64
        import hashlib
        header = (ROOT / 'hid/ck_provision/checked_command.h').read_text()
        helper = header[header.index('String checkedPayload('):header.index('String checkedCommand(')]
        cpp = self.base / 'checked.cpp'
        cpp.write_text('#include <string>\n#include <iostream>\nusing String = std::string;\n' + helper +
            '\nint main(int argc, char **argv) { std::cout << checkedPayload(argv[1], argv[2]); }\n')
        binary = self.base / 'checked'
        built = subprocess.run(['c++', str(cpp), '-o', str(binary)], capture_output=True, text=True)
        self.assertEqual(built.returncode, 0, built.stderr)
        payload = b'printf executed > "$HOME/executed"'
        encoded = base64.b64encode(payload).decode()
        digest = hashlib.sha256(payload).hexdigest()
        for body, sha, runs in ((encoded, digest, True), (encoded[:-4], digest, False), (encoded, '0' * 64, False)):
            marker = self.base / 'executed'
            if marker.exists(): marker.unlink()
            command = subprocess.check_output([str(binary), body, sha], text=True)
            parsed = subprocess.run(['/bin/bash', '-n'], input=command, text=True, capture_output=True)
            self.assertEqual(parsed.returncode, 0, parsed.stderr)
            subprocess.run(['/bin/bash', '-c', command], env=self.env, capture_output=True)
            self.assertEqual(marker.exists(), runs)

    def test_windows_run_command_fits_and_keeps_the_initial_window_open(self):
        sketch = (ROOT / 'hid/ck_provision/ck_provision.ino').read_text()
        begin = sketch.index('    String cmd = String("powershell')
        command = sketch[begin:sketch.index('\n', begin)]
        cpp = self.base / 'windows-command.cpp'
        cpp.write_text('#include <string>\n#include <iostream>\nusing String = std::string;\n'
                       'const char *URL_PS = "https://raw.githubusercontent.com/TheDivyanshShukla/cluster-kit/main/scripts/provision.ps1";\n'
                       'int main() {\n' + command + '\nstd::cout << cmd;\n}\n')
        binary = self.base / 'windows-command'
        subprocess.run(['c++', str(cpp), '-o', str(binary)], check=True, capture_output=True)
        generated = subprocess.check_output([str(binary)], text=True)
        self.assertLessEqual(len(generated), 259, generated)
        self.assertIn('-NoExit', generated.split('-Command', 1)[0])
        self.assertNotIn('Start-Process', generated)
        self.assertIn('-ExecutionPolicy RemoteSigned', generated.split('-Command', 1)[0])


if __name__ == '__main__':
    unittest.main()
