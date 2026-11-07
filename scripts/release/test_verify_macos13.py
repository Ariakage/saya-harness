#!/usr/bin/env python3
"""Test macOS 13 evidence validation with synthetic archives and mocked OS calls."""
from contextlib import redirect_stdout, redirect_stderr
import hashlib
import io
import json
from pathlib import Path
import struct
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
import verify_macos13 as verify


def create_archive(path, target='macos-aarch64', cpu=0x0100000c):
    binary = b'\xcf\xfa\xed\xfe' + struct.pack('<I', cpu) + b'\x00' * 56
    metadata = {'target': target, 'binary_sha256': hashlib.sha256(binary).hexdigest(),
                'declared_source_commit': 'a' * 40}
    contents = {'saya': binary, 'LICENSE': b'license', 'README.md': b'readme',
                'BUILD-INFO.json': json.dumps(metadata).encode()}
    with tarfile.open(path, 'w:gz') as archive:
        for name, data in contents.items():
            member = tarfile.TarInfo(name)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    return hashlib.sha256(path.read_bytes()).hexdigest()


class MacOS13VerifierTest(unittest.TestCase):
    def test_evidence_and_refusal_cases(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'candidate.tar.gz'
            checksum = create_archive(archive)
            evidence = root / 'evidence.json'
            args = ['verify_macos13.py', str(archive), '--sha256', checksum, '--output', str(evidence)]
            def fake_command(*args):
                return {'sw_vers': '13.7.6', 'otool': 'minos 13.0', 'sysctl': '1'}[args[0]]
            with mock.patch.object(sys, 'argv', args), mock.patch.object(verify.platform, 'system', return_value='Darwin'), \
                 mock.patch.object(verify.platform, 'machine', return_value='arm64'), \
                 mock.patch.object(verify, 'command', side_effect=fake_command), \
                 mock.patch.object(verify.subprocess, 'run') as run, redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                self.assertEqual(verify.main(), 0)
                self.assertEqual(run.call_count, 2)
                result = json.loads(evidence.read_text())
                self.assertEqual(result['target'], 'macos-aarch64')
                self.assertEqual(result['archive_sha256'], checksum)
                self.assertEqual(result['interactive_journey'], 'passed')
                self.assertNotEqual(verify.main(), 0)
                self.assertEqual(json.loads(evidence.read_text()), result)
            wrong = root / 'wrong.tar.gz'
            wrong_checksum = create_archive(wrong, 'macos-x86_64', 0x0100000c)
            bad_output = root / 'bad.json'
            bad_args = ['verify_macos13.py', str(wrong), '--sha256', wrong_checksum, '--output', str(bad_output)]
            with mock.patch.object(sys, 'argv', bad_args), mock.patch.object(verify.platform, 'system', return_value='Darwin'), \
                 mock.patch.object(verify, 'command', side_effect=fake_command), \
                 mock.patch.object(verify.subprocess, 'run') as run, redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                self.assertNotEqual(verify.main(), 0)
                run.assert_not_called()
                self.assertFalse(bad_output.exists())
            with mock.patch.object(sys, 'argv', args), mock.patch.object(verify.platform, 'system', return_value='Darwin'), \
                 mock.patch.object(verify, 'command', return_value='27.2'), redirect_stderr(io.StringIO()):
                self.assertNotEqual(verify.main(), 0)


if __name__ == '__main__':
    unittest.main()
