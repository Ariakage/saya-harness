"""Namespace-independent lifetime witnesses for controlled POSIX fixtures."""

import os
from pathlib import Path
import stat
import time


class ProcessLease:
    """Observe a FIFO writer held by a fixture and inherited by its children."""

    READY = b'READY\n'

    def __init__(self, path):
        self.path = Path(path)
        os.mkfifo(self.path, 0o600)
        self._reader = None
        self._buffer = bytearray()
        self._activity_observed = False
        self._active_observed = False
        self._writers_present = False
        try:
            self._reader = os.open(self.path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
        except BaseException:
            self.path.unlink()
            raise
        identity = os.fstat(self._reader)
        self._identity = (identity.st_dev, identity.st_ino)

    def __enter__(self):
        return self

    def __exit__(self, *_arguments):
        self.close()

    def fileno(self):
        if self._reader is None:
            raise RuntimeError('process lease is closed')
        return self._reader

    def _observe(self):
        reader = self.fileno()
        while True:
            try:
                data = os.read(reader, 4096)
            except BlockingIOError:
                self._activity_observed = True
                self._writers_present = True
                break
            if not data:
                self._writers_present = False
                break
            self._activity_observed = True
            self._buffer.extend(data)
            if not self.READY.startswith(self._buffer):
                raise RuntimeError('unexpected process lease readiness bytes')
        if self._buffer == self.READY and self._writers_present:
            self._active_observed = True

    def ready(self):
        self._observe()
        return self._active_observed

    def active(self):
        self._observe()
        return self._active_observed and self._writers_present

    def started(self):
        self._observe()
        return self._activity_observed

    def released(self):
        self._observe()
        return self._active_observed and not self._writers_present

    def wait_ready(self, timeout=10):
        deadline = time.monotonic() + timeout
        while not self.ready():
            if time.monotonic() >= deadline:
                raise RuntimeError('no live process lease writer became ready')
            time.sleep(0.01)

    def wait_released(self, timeout=5):
        if not self._active_observed:
            raise RuntimeError('process cleanup lacks a live writer positive control')
        deadline = time.monotonic() + timeout
        while not self.released():
            if time.monotonic() >= deadline:
                raise RuntimeError('controlled process or inherited writer survived cleanup')
            time.sleep(0.01)

    def close(self):
        if self._reader is None:
            return
        os.close(self._reader)
        self._reader = None
        try:
            identity = self.path.lstat()
            if stat.S_ISFIFO(identity.st_mode) and (identity.st_dev, identity.st_ino) == self._identity:
                self.path.unlink()
        except FileNotFoundError:
            pass
