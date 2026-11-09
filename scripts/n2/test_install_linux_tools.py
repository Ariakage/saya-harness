#!/usr/bin/env python3
"""Offline regression checks for the installer's download and checksum gate."""

import hashlib
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest


INSTALLER = Path(__file__).with_name('install-linux-tools.sh')
VERSION = '0.0-controlled-fixture'
PRIMARY = ('https://github.com/containers/bubblewrap/releases/download/'
           f'v{VERSION}/bubblewrap-{VERSION}.tar.xz')
FALLBACK = 'https://api.github.com/repos/containers/bubblewrap/releases/assets/640310180'
ARCHIVE_BYTES = b'SAYA_SYNTHETIC_BUBBLEWRAP_ARCHIVE\x00\xff\n'


MOCK_COMMAND = r'''
import hashlib
import json
import os
from pathlib import Path
import re
import sys

root = Path(os.environ['SAYA_INSTALL_FIXTURE'])
configuration = json.loads((root / 'control.json').read_text())
tool, arguments = sys.argv[1], sys.argv[2:]
entry = {'tool': tool, 'arguments': arguments}

if tool == 'curl':
    url = arguments[-1]
    if url == configuration['primary_url']:
        source = 'primary'
    elif url == configuration['fallback_url']:
        source = 'fallback'
    else:
        raise SystemExit('unexpected download endpoint')
    outcome = configuration[source]
    output = Path(arguments[arguments.index('--output') + 1])
    output.write_bytes(bytes.fromhex(outcome['bytes']))
    entry.update(source=source, output=str(output), status=outcome['status'])
    status = outcome['status']
elif tool == 'sha256sum':
    if arguments != ['--check', '--strict', '-']:
        raise SystemExit('unexpected checksum invocation')
    line = sys.stdin.read()
    match = re.fullmatch(r'([0-9a-f]{64})  (.+)\n', line)
    if match is None:
        raise SystemExit('invalid checksum input')
    expected, archive = match.groups()
    actual = hashlib.sha256(Path(archive).read_bytes()).hexdigest()
    status = 0 if actual == expected else 1
    entry.update(expected=expected, actual=actual, status=status)
    print(archive + (': OK' if status == 0 else ': FAILED'))
elif tool == 'tar':
    if arguments != ['-xf', configuration['archive'], '-C', configuration['build_root']]:
        raise SystemExit('unexpected extraction invocation')
    (root / 'extraction-reached').write_text('controlled extraction marker\n')
    status = 0
else:
    raise SystemExit('unexpected external command')

with (root / 'commands.jsonl').open('a') as log:
    log.write(json.dumps(entry) + '\n')
raise SystemExit(status)
'''


def download_block():
    lines = INSTALLER.read_text().splitlines()
    start = lines.index('archive="$build_root/bubblewrap.tar.xz"')
    end = lines.index('tar -xf "$archive" -C "$build_root"', start)
    return '\n'.join(lines[start:end + 1]) + '\n'


class InstallerDownloadTests(unittest.TestCase):
    def run_download(self, primary_status=0, fallback_status=0, fallback_bytes=ARCHIVE_BYTES):
        temporary = tempfile.TemporaryDirectory(prefix='saya-installer-test-')
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        build_root = root / 'build root'
        build_root.mkdir()
        archive = build_root / 'bubblewrap.tar.xz'
        configuration = {
            'primary_url': PRIMARY,
            'fallback_url': FALLBACK,
            'archive': str(archive),
            'build_root': str(build_root),
            'primary': {
                'status': primary_status,
                'bytes': (ARCHIVE_BYTES if primary_status == 0 else b'partial primary').hex(),
            },
            'fallback': {'status': fallback_status, 'bytes': fallback_bytes.hex()},
        }
        (root / 'control.json').write_text(json.dumps(configuration))
        helper = root / 'mock_command.py'
        helper.write_text(MOCK_COMMAND)
        executables = root / 'bin'
        executables.mkdir()
        for tool in ('curl', 'sha256sum', 'tar'):
            executable = executables / tool
            executable.write_text('#!/bin/sh\nexec ' + shlex.join([
                sys.executable, str(helper), tool]) + ' "$@"\n')
            executable.chmod(0o700)
        checksum = hashlib.sha256(ARCHIVE_BYTES).hexdigest()
        prefix = '\n'.join([
            'build_root=' + shlex.quote(str(build_root)),
            'version=' + shlex.quote(VERSION),
            'checksum=' + shlex.quote(checksum),
        ]) + '\n'
        # The restricted PATH contains only the three offline fixture commands.
        # Extraction is a marker, while checksum verification hashes real bytes.
        result = subprocess.run(['/bin/sh', '-eu', '-c', prefix + download_block()],
                                cwd=root, env={'PATH': str(executables),
                                               'SAYA_INSTALL_FIXTURE': str(root),
                                               'PYTHONUTF8': '1'},
                                capture_output=True, text=True, timeout=10)
        log = root / 'commands.jsonl'
        commands = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
        return result, root, archive, commands

    def test_primary_success_checks_bytes_before_extraction(self):
        result, root, archive, commands = self.run_download()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([entry['tool'] for entry in commands], ['curl', 'sha256sum', 'tar'])
        self.assertEqual(commands[0]['source'], 'primary')
        self.assertEqual(commands[0]['arguments'][-1], PRIMARY)
        self.assertEqual(archive.read_bytes(), ARCHIVE_BYTES)
        self.assertEqual(commands[1]['actual'], hashlib.sha256(ARCHIVE_BYTES).hexdigest())
        self.assertEqual(commands[1]['actual'], commands[1]['expected'])
        self.assertTrue((root / 'extraction-reached').is_file())

    def test_primary_failure_uses_official_api_and_replaces_partial_download(self):
        result, root, archive, commands = self.run_download(primary_status=22)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([entry['tool'] for entry in commands], ['curl', 'curl', 'sha256sum', 'tar'])
        self.assertEqual([entry['source'] for entry in commands[:2]], ['primary', 'fallback'])
        self.assertEqual(commands[0]['output'], commands[1]['output'])
        arguments = commands[1]['arguments']
        self.assertEqual(arguments[-1], FALLBACK)
        self.assertIn('Accept: application/octet-stream', arguments)
        self.assertIn('X-GitHub-Api-Version: 2022-11-28', arguments)
        self.assertFalse(any('authorization' in value.lower() for value in arguments))
        self.assertEqual(archive.read_bytes(), ARCHIVE_BYTES)
        self.assertEqual(commands[2]['actual'], commands[2]['expected'])
        self.assertTrue((root / 'extraction-reached').is_file())

    def test_fallback_bad_checksum_stops_before_extraction(self):
        corrupted = b'SAYA_SYNTHETIC_CORRUPTED_ARCHIVE'
        result, root, archive, commands = self.run_download(primary_status=22, fallback_bytes=corrupted)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([entry['tool'] for entry in commands], ['curl', 'curl', 'sha256sum'])
        self.assertEqual(archive.read_bytes(), corrupted)
        self.assertEqual(commands[-1]['actual'], hashlib.sha256(corrupted).hexdigest())
        self.assertNotEqual(commands[-1]['actual'], commands[-1]['expected'])
        self.assertEqual(commands[-1]['status'], 1)
        self.assertFalse((root / 'extraction-reached').exists())

    def test_both_downloads_fail_without_checksum_or_extraction(self):
        result, root, _archive, commands = self.run_download(primary_status=22, fallback_status=22)
        self.assertEqual(result.returncode, 22)
        self.assertEqual([entry['tool'] for entry in commands], ['curl', 'curl'])
        self.assertEqual([entry['status'] for entry in commands], [22, 22])
        self.assertFalse((root / 'extraction-reached').exists())


if __name__ == '__main__':
    unittest.main()
