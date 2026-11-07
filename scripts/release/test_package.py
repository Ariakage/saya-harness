#!/usr/bin/env python3
"""Tests for local candidate packaging; no compiler or network is required."""
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = ROOT / 'scripts/release/package.py'


def elf(machine):
    return b'\x7fELF\x02\x01' + b'\x00' * 12 + struct.pack('<H', machine) + b'\x00' * 44


def macho(cpu):
    return b'\xcf\xfa\xed\xfe' + struct.pack('<I', cpu) + b'\x00' * 56


class CandidatePackageTest(unittest.TestCase):
    def test_architecture_validation_determinism_and_no_overwrite(self):
        commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = {'linux-x86_64': elf(62), 'linux-aarch64': elf(183),
                        'macos-x86_64': macho(0x01000007), 'macos-aarch64': macho(0x0100000c)}
            args = []
            for target, data in binaries.items():
                path = root / target
                path.write_bytes(data)
                args += ['--binary', target + '=' + str(path)]
            def package(output, supplied=args):
                return subprocess.run([sys.executable, str(PACKAGE), '--source-commit', commit,
                    '--output', str(output), *supplied], cwd=ROOT, capture_output=True)
            first, second = root / 'first', root / 'second'
            self.assertEqual(package(first).returncode, 0)
            self.assertEqual(package(second).returncode, 0)
            before = (first / 'SHA256SUMS').read_bytes()
            self.assertEqual(before, (second / 'SHA256SUMS').read_bytes())
            for line in before.decode().splitlines():
                checksum, name = line.split('  ', 1)
                blob = (first / name).read_bytes()
                self.assertEqual(hashlib.sha256(blob).hexdigest(), checksum)
                self.assertEqual(blob, (second / name).read_bytes())
                with tarfile.open(first / name, 'r:gz') as archive:
                    info = json.load(archive.extractfile('BUILD-INFO.json'))
                    binary = archive.extractfile('saya').read()
                    self.assertEqual(info['declared_source_commit'], commit)
                    self.assertEqual(hashlib.sha256(binary).hexdigest(), info['binary_sha256'])
            self.assertNotEqual(package(first).returncode, 0)
            self.assertEqual((first / 'SHA256SUMS').read_bytes(), before)
            (root / 'linux-x86_64').write_bytes(elf(183))
            invalid = root / 'invalid'
            self.assertNotEqual(package(invalid).returncode, 0)
            self.assertFalse(invalid.exists())


if __name__ == '__main__':
    unittest.main()
