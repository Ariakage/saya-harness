#!/usr/bin/env python3
"""Opt-in four-mode live sample; sends real model requests and may incur costs.

Set SAYA_LIVE_API_KEY, SAYA_LIVE_BASE_URL, SAYA_LIVE_MODEL and
SAYA_LIVE_SANDBOX_IMAGE. The selected endpoint receives the credential. The
Docker image must contain rustc. Configure DOCKER_HOST as needed. Generated Rust
runs without network or host mounts in a restricted Linux container; containers
are not a guarantee against host kernel escapes.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

BUGGY = '''use std::io::{self, Read};
fn main() {
    let mut text = String::new();
    io::stdin().read_to_string(&mut text).unwrap();
    println!("lines={} words={}", text.lines().count(), text.trim().split(' ').count());
}
'''


def snapshot(workspace):
    return {
        path.relative_to(workspace).as_posix(): path.read_bytes() if path.is_file() else None
        for path in workspace.rglob('*')
    }


def sandbox_check(path, image):
    command = [
        'docker', 'run', '--rm', '-i', '--network', 'none', '--read-only',
        '--tmpfs', '/tmp:rw,exec,nosuid,nodev,size=64m', '--cap-drop', 'ALL',
        '--security-opt', 'no-new-privileges', '--pids-limit', '64', '--memory', '512m',
        '--platform', os.environ.get('SAYA_LIVE_SANDBOX_PLATFORM', 'linux/arm64'), image,
        'sh', '-c', 'cat > /tmp/main.rs && rustc --edition=2024 /tmp/main.rs -o /tmp/test '
        '&& printf "alpha  beta\\ngamma delta\\n" | /tmp/test',
    ]
    docker_env = os.environ.copy()
    docker_env.pop('SAYA_LIVE_API_KEY', None)
    try:
        result = subprocess.run(command, input=path.read_bytes(), env=docker_env,
                                capture_output=True, timeout=40)
        return result.returncode == 0 and result.stdout.strip() == b'lines=2 words=4'
    except (OSError, subprocess.TimeoutExpired):
        return False


def main():
    try:
        secret = os.environ['SAYA_LIVE_API_KEY']
        api_root = os.environ['SAYA_LIVE_BASE_URL']
        model = os.environ['SAYA_LIVE_MODEL']
        image = os.environ['SAYA_LIVE_SANDBOX_IMAGE']
    except KeyError as error:
        print('Missing required environment variable:', error.args[0], file=sys.stderr)
        return 2
    binary = Path('target/release/saya').resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='saya-live-modes-') as temporary:
        root = Path(temporary)
        env = {key: os.environ[key] for key in (
            'PATH', 'HTTPS_PROXY', 'HTTP_PROXY', 'ALL_PROXY', 'NO_PROXY',
            'SSL_CERT_FILE', 'SSL_CERT_DIR') if key in os.environ}
        env.update({'HOME': str(root), 'XDG_DATA_HOME': str(root / 'data'),
                    'XDG_STATE_HOME': str(root / 'state'), 'XDG_CACHE_HOME': str(root / 'cache'),
                    'SAYA_LIVE_API_KEY': secret})
        config = root / 'config.toml'
        config.write_text('\n'.join([
            'schema_version = 1', '[defaults]', 'profile = "live"', '[profiles.live]',
            'protocol = "openai-chat-completions"', 'base_url = ' + json.dumps(api_root),
            'model = ' + json.dumps(model), 'auth = "bearer"', 'api_key_env = "SAYA_LIVE_API_KEY"',
            'context_window_tokens = 128000', 'max_output_tokens = 8192', 'stream = true',
            'legacy_max_tokens = true', '[limits]', 'max_model_requests = 10', 'max_tool_calls = 24',
            '[[permissions.rules]]', 'effect = "deny"', 'tool = "exec"', '']), encoding='utf-8')

        def run(mode, prompt, files):
            workspace = root / mode
            workspace.mkdir()
            for name, content in files.items():
                path = workspace / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding='utf-8')
            before = snapshot(workspace)
            command = [str(binary), '--config', str(config), '--cwd', str(workspace),
                       '--mode', mode, '--output', 'jsonl', 'run', prompt]
            try:
                result = subprocess.run(command, env=env, cwd=workspace, stdin=subprocess.DEVNULL,
                                        capture_output=True, timeout=200)
                events = [json.loads(line) for line in result.stdout.decode().splitlines()]
                end = next((event for event in reversed(events) if event.get('type') == 'run.finished'), {})
                data = end.get('data', {})
                calls = [event.get('data', {}) for event in events if event.get('type') == 'tool.finished']
                ok = result.returncode == 0 and data.get('status') == 'completed'
                return workspace, before, ok, calls, result.returncode, data.get('text', '')
            except (OSError, subprocess.TimeoutExpired, UnicodeError, json.JSONDecodeError):
                return workspace, before, False, [], None, ''

        results = []
        workspace, before, ok, calls, code, text = run('plan',
            'Read src/main.rs with the read_file tool, explain its whitespace counting bug and give a '
            'concise three-step test plan in under 150 words. Do not write files or run commands.',
            {'src/main.rs': BUGGY})
        ok = (ok and before == snapshot(workspace) and 'whitespace' in text.lower()
              and ('test' in text.lower() or 'plan' in text.lower())
              and any(call.get('name') == 'read_file' and call.get('result', {}).get('ok') for call in calls))
        results.append(('PLAN read and unchanged workspace', ok, code))

        workspace, _, ok, _, code, _ = run('build',
            'Create src/main.rs as a small Rust program that reads stdin and prints exactly lines=N words=M, '
            'with lines counted by str::lines and words by split_whitespace. Do not run commands; '
            'an external evaluator compiles and tests it.', {'src/.keep': ''})
        path = workspace / 'src/main.rs'
        ok = ok and path.is_file() and sandbox_check(path, image)
        results.append(('BUILD implementation and isolated execution', ok, code))

        workspace, _, ok, _, code, _ = run('code',
            'Read src/main.rs and fix its word count so arbitrary runs of whitespace are handled using '
            'split_whitespace, preserving its stdin and output interface. Do not run commands; '
            'an external evaluator compiles and tests it.', {'src/main.rs': BUGGY})
        path = workspace / 'src/main.rs'
        ok = ok and path.is_file() and path.read_text() != BUGGY and sandbox_check(path, image)
        results.append(('CODE repair and isolated execution', ok, code))

        workspace, _, ok, _, code, _ = run('work',
            'Read the three input files and write report.md containing each colour and its number plus '
            'the total. Do not run commands.', {'red.txt': 'red 4\n', 'blue.txt': 'blue 7\n', 'green.txt': 'green 2\n'})
        report = workspace / 'report.md'
        content = report.read_text().lower() if report.is_file() else ''
        ok = ok and all(part in content for part in ('red', 'blue', 'green', '4', '7', '2', '13'))
        results.append(('WORK three-source report', ok, code))
        for name, ok, code in results:
            print(f'{name}: {"passed" if ok else "failed"} (exit={code})', flush=True)
        return int(not all(result[1] for result in results))


if __name__ == '__main__':
    sys.exit(main())
