"""Provisioning logic runs against temporary repos and fake OS commands, never this host."""
import os
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

    def test_firmware_bootstrap_installs_curl_before_download(self):
        header = ROOT / 'hid/ck_provision/ubuntu_bootstrap.h'
        self.assertTrue(header.exists(), 'Firmware needs a dependency-free bootstrap')
        code = header.read_text().split('R"CK(')[1].split(')CK"')[0]
        commands = self.base / 'commands'
        commands.mkdir()
        # A restricted PATH makes curl genuinely missing until the fake package manager installs it.
        for name in ('mktemp', 'rm', 'bash'):
            target = subprocess.run(['which', name], text=True, capture_output=True, check=True).stdout.strip()
            (commands / name).symlink_to(target)
        curl = commands / 'curl'
        payload = '#!/bin/bash\nprintf "downloaded-script-ran\\n"\n'
        (commands / 'apt-get').write_text(f'''#!/bin/bash
printf 'apt %s\\n' "$*" >> "$HOME/calls"
case " $* " in
  *' --fix-broken install '*) : > "$HOME/repaired" ;;
  *' install curl ca-certificates '*)
    if [ ! -e "$HOME/failed-once" ]; then : > "$HOME/failed-once"; exit 1; fi
    cat > '{curl}' <<'CURL'
#!/bin/bash
printf 'download\\n' >> "$HOME/calls"
while [ "$#" -gt 0 ]; do if [ "$1" = -o ]; then shift; printf '%s' '{payload}' > "$1"; exit 0; fi; shift; done
exit 1
CURL
    chmod +x '{curl}' ;;
esac
'''.replace('    cat >', '    /bin/cat >').replace('    chmod +x', '    /bin/chmod +x'))
        (commands / 'dpkg').write_text('#!/bin/bash\nprintf "dpkg %s\\n" "$*" >> "$HOME/calls"\n[ -f "$HOME/repaired" ]\n')
        for item in ('apt-get', 'dpkg'):
            (commands / item).chmod(0o755)
        result = subprocess.run(['/bin/bash', '-c', code, 'ck-bootstrap', 'https://example.invalid/provision.sh'], cwd=self.base, env=dict(self.env, PATH=str(commands)), text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('downloaded-script-ran', result.stdout)
        calls = (self.base / 'calls').read_text()
        self.assertIn('dpkg --configure -a', calls)
        self.assertIn('--fix-broken install', calls)
        self.assertLess(calls.index('install curl ca-certificates'), calls.index('download'))

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
        command = sketch[begin:sketch.index('    typeText(cmd);', begin)]
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


if __name__ == '__main__':
    unittest.main()
