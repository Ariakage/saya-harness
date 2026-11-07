#!/usr/bin/env python3
"""Opt-in live CLI smoke; no secrets, responses, or task data are printed or saved.

The caller supplies SAYA_LIVE_API_KEY and three protocol roots. Requests may incur
charges. This checks one model/provider, not broad model quality or compatibility.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import uuid


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--model', required=True)
    parser.add_argument('--chat-root', required=True)
    parser.add_argument('--responses-root', required=True)
    parser.add_argument('--anthropic-root', required=True)
    parser.add_argument('--context-window', type=int, required=True)
    args = parser.parse_args()
    secret = os.environ.get('SAYA_LIVE_API_KEY')
    if not secret:
        parser.error('SAYA_LIVE_API_KEY must be set in the environment')
    binary = args.binary.resolve(strict=True)
    cases = [
        ('chat', 'openai-chat-completions', args.chat_root, 'legacy_max_tokens = true'),
        ('responses', 'openai-responses', args.responses_root, ''),
        ('anthropic', 'anthropic-messages', args.anthropic_root, 'api_version = "2026-10-21"'),
    ]
    failed = False
    with tempfile.TemporaryDirectory(prefix='saya-live-smoke-') as temporary:
        root = Path(temporary)
        workspace = root / 'workspace'
        workspace.mkdir()
        marker = 'SAYA_SMOKE_' + uuid.uuid4().hex
        (workspace / 'fact.txt').write_text(marker + '\n', encoding='utf-8')
        env = {key: os.environ[key] for key in ('PATH', 'SSL_CERT_FILE', 'SSL_CERT_DIR', 'HTTPS_PROXY', 'HTTP_PROXY', 'ALL_PROXY', 'NO_PROXY') if key in os.environ}
        env.update({'HOME': str(root), 'XDG_DATA_HOME': str(root / 'data'),
                    'XDG_STATE_HOME': str(root / 'state'), 'XDG_CACHE_HOME': str(root / 'cache'),
                    'SAYA_LIVE_API_KEY': secret})
        for name, protocol, api_root, extra in cases:
            # JSON-quoted strings are also TOML basic strings.
            config = root / (name + '.toml')
            config.write_text('\n'.join([
                'schema_version = 1', '[defaults]', 'profile = "live"',
                '[profiles.live]', 'protocol = ' + json.dumps(protocol),
                'base_url = ' + json.dumps(api_root), 'model = ' + json.dumps(args.model),
                'auth = "bearer"', 'api_key_env = "SAYA_LIVE_API_KEY"',
                'context_window_tokens = ' + str(args.context_window),
                'max_output_tokens = 1024', 'stream = true', extra, ''
            ]), encoding='utf-8')
            for task, prompt in [
                ('direct', 'Reply exactly with the single word READY.'),
                ('read_tool', 'Use the read_file tool to read fact.txt, then reply with the exact entire file content. Do not guess.'),
            ]:
                try:
                    result = subprocess.run([str(binary), '--config', str(config), '--cwd', str(workspace),
                        '--mode', 'plan', '--output', 'jsonl', 'run', prompt], env=env,
                        cwd=workspace, stdin=subprocess.DEVNULL, capture_output=True, timeout=180)
                    events = [json.loads(line) for line in result.stdout.decode('utf-8').splitlines()]
                    finished = next((e for e in reversed(events) if e.get('type') == 'run.finished'), {})
                    text = ''.join(e.get('data', {}).get('text', '') for e in events if e.get('type') == 'model.text_delta')
                    # The final event also contains the final text. Search parsed values only in memory.
                    final_text = json.dumps(finished, ensure_ascii=False)
                    tools = [e for e in events if e.get('type') == 'tool.finished' and e.get('data', {}).get('name') == 'read_file' and e.get('data', {}).get('result', {}).get('ok') is True]
                    models = [e for e in events if e.get('type') == 'model.finished' and e.get('data', {}).get('ok') is True]
                    completed = result.returncode == 0 and finished.get('data', {}).get('status') == 'completed'
                    if task == 'direct':
                        ok = completed and 'READY' in (text + final_text)
                    else:
                        ok = completed and marker in (text + final_text) and len(tools) >= 1 and len(models) >= 2
                    print(f'{name}/{task}: {"passed" if ok else "failed"} (exit={result.returncode}, tool_results={len(tools)}, model_responses={len(models)})', flush=True)
                    if not ok:
                        failed = True
                except (OSError, subprocess.TimeoutExpired, UnicodeError, json.JSONDecodeError) as error:
                    print(f'{name}/{task}: failed ({type(error).__name__})', flush=True)
                    failed = True
        if (workspace / 'fact.txt').read_text(encoding='utf-8') != marker + '\n' or len(list(workspace.iterdir())) != 1:
            print('workspace_unchanged: failed')
            failed = True
        else:
            print('workspace_unchanged: passed')
    return int(failed)


if __name__ == '__main__':
    sys.exit(main())
