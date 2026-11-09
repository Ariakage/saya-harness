//! Private launcher image and bounded, descriptor-only payload environment.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{FileExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::Path,
    sync::OnceLock,
};
const ENV_MAX_BYTES: usize = 8 * 1024 * 1024;
const ENV_MAGIC: &[u8; 8] = b"SAYAENV1";
struct Image {
    file: File,
    fingerprint: (u64, u64, u64, i64, i64, i64, i64),
}
static IMAGE: OnceLock<Result<Image, io::Error>> = OnceLock::new();
/// Pin before running any model or tool. Replaced pathname entries cannot
/// change the image used by subsequent internal workers in this process.
pub fn pin_launcher_image() -> io::Result<()> {
    image().map(|_| ())
}
/// Select a trusted internal CLI image before the first pin. This is a host
/// integration API for GUI frontends and controlled test fixtures, never model input.
pub fn pin_launcher_image_from(path: &Path) -> io::Result<()> {
    let file = above_stdio(File::open(path)?)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    let candidate = Image {
        fingerprint: fingerprint(&metadata),
        file,
    };
    let identity = candidate.fingerprint;
    match IMAGE.set(Ok(candidate)) {
        Ok(()) => Ok(()),
        Err(_) => match IMAGE.get() {
            Some(Ok(image)) if image.fingerprint == identity => Ok(()),
            _ => Err(io::Error::from_raw_os_error(libc::EALREADY)),
        },
    }
}
fn image() -> io::Result<&'static Image> {
    match IMAGE.get_or_init(|| {
        #[cfg(target_os = "linux")]
        let file = File::open("/proc/self/exe")?;
        #[cfg(target_os = "macos")]
        let file = File::open(std::env::current_exe()?)?;
        let file = above_stdio(file)?;
        let fingerprint = fingerprint(&file.metadata()?);
        Ok(Image { file, fingerprint })
    }) {
        Ok(image) => Ok(image),
        Err(error) => Err(io::Error::from_raw_os_error(
            error.raw_os_error().unwrap_or(libc::EIO),
        )),
    }
}
fn fingerprint(metadata: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}
pub(super) fn copy_launcher(destination: &Path) -> io::Result<()> {
    let image = image()?;
    if fingerprint(&image.file.metadata()?) != image.fingerprint
        || image.fingerprint.2 > 1024 * 1024 * 1024
    {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o500)
        .open(destination)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut offset = 0;
    while offset < image.fingerprint.2 {
        let count = image.file.read_at(&mut buffer, offset)?;
        if count == 0 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        target.write_all(&buffer[..count])?;
        offset += count as u64;
    }
    if fingerprint(&image.file.metadata()?) != image.fingerprint {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    target.sync_all()?;
    Ok(())
}
fn above_stdio(file: File) -> io::Result<File> {
    if file.as_raw_fd() >= 3 {
        return Ok(file);
    }
    // SAFETY: file owns a live fd. The returned duplicate has independent fd ownership.
    let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}
pub(super) fn environment_file(
    environment: &BTreeMap<OsString, OsString>,
    directory: &Path,
) -> io::Result<File> {
    let mut bytes = Vec::from(ENV_MAGIC.as_slice());
    let count =
        u32::try_from(environment.len()).map_err(|_| io::Error::from_raw_os_error(libc::E2BIG))?;
    if count > 1_048_576 {
        return Err(io::Error::from_raw_os_error(libc::E2BIG));
    }
    bytes.extend_from_slice(&count.to_be_bytes());
    for (name, value) in environment {
        let name = name.as_bytes();
        let value = value.as_bytes();
        if name.is_empty() || name.contains(&b'=') || name.contains(&0) || value.contains(&0) {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let total = bytes
            .len()
            .checked_add(8)
            .and_then(|size| size.checked_add(name.len()))
            .and_then(|size| size.checked_add(value.len()))
            .filter(|size| *size <= ENV_MAX_BYTES)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::E2BIG))?;
        bytes
            .try_reserve(total - bytes.len())
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
        bytes.extend_from_slice(&(name.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(value);
    }
    let path = directory.join(format!("environment-{}", uuid::Uuid::now_v7()));
    let opened = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    // Unlink before any values are written, including on a later serialization error.
    fs::remove_file(path)?;
    let mut file = above_stdio(opened)?;
    file.write_all(&bytes)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}
pub(super) fn read_environment(descriptor: i32) -> io::Result<BTreeMap<OsString, OsString>> {
    if descriptor < 3 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: this private bootstrap takes ownership of the explicitly inherited fd.
    let file = unsafe { File::from_raw_fd(descriptor) };
    if !file.metadata()?.is_file() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let mut bytes = Vec::new();
    file.take(ENV_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > ENV_MAX_BYTES || !bytes.starts_with(ENV_MAGIC) {
        return Err(io::Error::from_raw_os_error(libc::E2BIG));
    }
    let mut cursor = 8;
    let count = integer(&bytes, &mut cursor)?;
    if count > 1_048_576 {
        return Err(io::Error::from_raw_os_error(libc::E2BIG));
    }
    let mut environment = BTreeMap::new();
    for _ in 0..count {
        let name_len = integer(&bytes, &mut cursor)? as usize;
        let value_len = integer(&bytes, &mut cursor)? as usize;
        let name = take(&bytes, &mut cursor, name_len)?;
        let value = take(&bytes, &mut cursor, value_len)?;
        if name.is_empty() || name.contains(&b'=') || name.contains(&0) || value.contains(&0) {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        if environment
            .insert(
                OsString::from_vec(name.to_vec()),
                OsString::from_vec(value.to_vec()),
            )
            .is_some()
        {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
    }
    if cursor != bytes.len() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    Ok(environment)
}
fn integer(bytes: &[u8], cursor: &mut usize) -> io::Result<u32> {
    let value = take(bytes, cursor, 4)?;
    Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}
fn take<'a>(bytes: &'a [u8], cursor: &mut usize, length: usize) -> io::Result<&'a [u8]> {
    let end = cursor
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    let value = &bytes[*cursor..end];
    *cursor = end;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::IntoRawFd;

    fn decode(bytes: &[u8]) -> io::Result<BTreeMap<OsString, OsString>> {
        let mut file = tempfile::tempfile()?;
        file.write_all(bytes)?;
        file.seek(SeekFrom::Start(0))?;
        read_environment(file.into_raw_fd())
    }

    fn wire(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut bytes = ENV_MAGIC.to_vec();
        bytes.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (key, value) in entries {
            bytes.extend_from_slice(&(key.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
            bytes.extend_from_slice(key);
            bytes.extend_from_slice(value);
        }
        bytes
    }

    #[test]
    fn private_environment_roundtrips_raw_unix_bytes_and_unlinks_before_write() {
        let directory = tempfile::tempdir().expect("private directory");
        let environment = BTreeMap::from([
            (OsString::from("UNICODE"), OsString::from("中文 🦀")),
            (OsString::from("EMPTY"), OsString::new()),
            (
                OsString::from_vec(vec![b'K', 0xff]),
                OsString::from_vec(vec![0xfe, b'v']),
            ),
        ]);
        let file = environment_file(&environment, directory.path()).expect("private descriptor");
        assert_eq!(file.metadata().expect("metadata").nlink(), 0);
        assert_eq!(file.metadata().expect("metadata").mode() & 0o777, 0o600);
        assert_eq!(
            fs::read_dir(directory.path()).expect("directory").count(),
            0
        );
        let witness = file.try_clone().expect("descriptor identity witness");
        let mut identity = std::mem::MaybeUninit::<libc::stat>::uninit();
        assert_eq!(
            unsafe { libc::fstat(witness.as_raw_fd(), identity.as_mut_ptr()) },
            0
        );
        let identity = unsafe { identity.assume_init() };
        let descriptor = file.into_raw_fd();
        assert_eq!(read_environment(descriptor).expect("decode"), environment);
        // Concurrent tests may reuse the descriptor number after it is closed.
        // The witness keeps the original inode alive for an unambiguous identity check.
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(descriptor, metadata.as_mut_ptr()) } == 0 {
            let metadata = unsafe { metadata.assume_init() };
            assert_ne!(
                (metadata.st_dev, metadata.st_ino),
                (identity.st_dev, identity.st_ino)
            );
        } else {
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        }
    }

    #[test]
    fn environment_decoder_rejects_malformed_and_ambiguous_frames() {
        let valid = wire(&[(b"KEY", b"value")]);
        for end in 0..valid.len() {
            assert!(
                decode(&valid[..end]).is_err(),
                "accepted truncation at {end}"
            );
        }
        let mut wrong_magic = valid.clone();
        wrong_magic[0] = b'X';
        assert!(decode(&wrong_magic).is_err());
        let mut trailing = valid;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        for entries in [
            vec![
                (b"KEY".as_slice(), b"first".as_slice()),
                (b"KEY", b"second"),
            ],
            vec![(b"".as_slice(), b"value".as_slice())],
            vec![(b"K=Y".as_slice(), b"value".as_slice())],
            vec![(b"K\0Y".as_slice(), b"value".as_slice())],
            vec![(b"KEY".as_slice(), b"v\0lue".as_slice())],
        ] {
            assert!(decode(&wire(&entries)).is_err());
        }
        let mut excessive_count = ENV_MAGIC.to_vec();
        excessive_count.extend_from_slice(&1_048_577u32.to_be_bytes());
        assert!(decode(&excessive_count).is_err());
        let mut impossible_length = wire(&[(b"KEY", b"value")]);
        impossible_length[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode(&impossible_length).is_err());
    }

    #[test]
    fn environment_budget_is_exact_and_rejected_without_a_disk_entry() {
        let directory = tempfile::tempdir().expect("private directory");
        // Header 12, entry lengths 8, key 1, with the remaining bytes as value.
        let value = OsString::from_vec(vec![b'x'; ENV_MAX_BYTES - 21]);
        let mut environment = BTreeMap::from([(OsString::from("K"), value)]);
        let file = environment_file(&environment, directory.path()).expect("exact limit");
        assert_eq!(
            file.metadata().expect("metadata").len(),
            ENV_MAX_BYTES as u64
        );
        assert_eq!(
            read_environment(file.into_raw_fd()).expect("decode"),
            environment
        );
        environment.insert(
            OsString::from("K"),
            OsString::from_vec(vec![b'x'; ENV_MAX_BYTES - 20]),
        );
        assert_eq!(
            environment_file(&environment, directory.path())
                .expect_err("above limit")
                .raw_os_error(),
            Some(libc::E2BIG)
        );
        assert_eq!(
            fs::read_dir(directory.path()).expect("directory").count(),
            0
        );
        let oversized = vec![b'x'; ENV_MAX_BYTES + 1];
        assert!(decode(&oversized).is_err());
    }
}
