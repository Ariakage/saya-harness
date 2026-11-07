#!/usr/bin/env python3
"""Validate Linux acceptance gates with synthetic archives and mocked OS calls."""
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
import verify_linux as verify


class LinuxVerifierTest(unittest.TestCase):
    def test_source_platform_hash_and_library_requirements(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'candidate.tar.gz'
            binary = b'\x7fELF\x02\x01' + b'\x00' * 12 + struct.pack('<H', 183) + b'\x00' * 44
            metadata = {'target': 'linux-aarch64', 'binary_sha256': hashlib.sha256(binary).hexdigest(),
                        'declared_source_commit': 'a' * 40}
            contents = {'saya': binary, 'LICENSE': b'license', 'README.md': b'readme',
                        'BUILD-INFO.json': json.dumps(metadata).encode()}
            with tarfile.open(archive, 'w:gz') as tar:
                for name, data in contents.items():
                    member = tarfile.TarInfo(name)
                    member.size = len(data)
                    tar.addfile(member, io.BytesIO(data))
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            output = root / 'evidence.json'
            arguments = ['verify_linux.py', str(archive), '--sha256', checksum,
                         '--source-commit', 'a' * 40, '--output', str(output)]
            def fake_command(*args):
                return {'objdump': 'GLIBC_2.17 GLIBC_2.34', 'ldd': 'libc.so.6 => /lib/libc.so.6',
                        'rustc': 'rustc 1.99.1'}[args[0]]
            with mock.patch.object(sys, 'argv', arguments), \
                 mock.patch.object(verify.platform, 'system', return_value='Linux'), \
                 mock.patch.object(verify.platform, 'machine', return_value='aarch64'), \
                 mock.patch.object(verify, 'os_release', return_value={'ID': 'ubuntu', 'VERSION_ID': '22.04'}), \
                 mock.patch.object(verify.os, 'confstr', return_value='glibc 2.35'), \
                 mock.patch.object(verify, 'command', side_effect=fake_command), \
                 mock.patch.object(verify.subprocess, 'run') as run, \
                 redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                self.assertEqual(verify.main(), 0)
                self.assertEqual(run.call_count, 4)
                evidence = json.loads(output.read_text())
                self.assertEqual(evidence['maximum_glibc_symbol'], '2.34')
                self.assertEqual(evidence['tui_journey'], 'passed')
                self.assertIn('tui_journey.py', evidence['test_script_sha256'])
                self.assertEqual(evidence['archive_sha256'], checksum)
                self.assertNotEqual(verify.main(), 0)
                self.assertEqual(json.loads(output.read_text()), evidence)
                run.reset_mock()
                bad_arguments = arguments.copy()
                bad_arguments[bad_arguments.index('--source-commit') + 1] = 'b' * 40
                with mock.patch.object(sys, 'argv', bad_arguments):
                    self.assertNotEqual(verify.main(), 0)
                    run.assert_not_called()
                with mock.patch.object(verify.os, 'confstr', return_value='glibc 2.36'):
                    self.assertNotEqual(verify.main(), 0)
                    run.assert_not_called()
                with mock.patch.object(verify.platform, 'machine', return_value='x86_64'):
                    self.assertNotEqual(verify.main(), 0)
                    run.assert_not_called()
                def new_glibc(*args):
                    return 'GLIBC_2.36' if args[0] == 'objdump' else fake_command(*args)
                with mock.patch.object(verify, 'command', side_effect=new_glibc):
                    self.assertNotEqual(verify.main(), 0)
                    run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
