//! Port of file.hh/file.cc and hash.cc

use crate::exception::{Error, ErrorKind, Result};
use crate::string_utils::prefix_match;
use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;

pub fn file_access_error(filename: &[u8], error_desc: &[u8]) -> Error {
    Error::new(ErrorKind::FileAccess, kformat!("{}: {}", filename, error_desc))
}

pub fn fd_access_error(fd: i32, error_desc: &[u8]) -> Error {
    Error::new(ErrorKind::FileAccess, kformat!("fd {}: {}", fd, error_desc))
}

pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

pub fn strerror(err: i32) -> Vec<u8> {
    unsafe { CStr::from_ptr(libc::strerror(err)).to_bytes().to_vec() }
}

pub fn last_error_str() -> Vec<u8> {
    strerror(errno())
}

pub fn cstr(s: &[u8]) -> CString {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    CString::new(&s[..end]).unwrap()
}

pub fn getenv(name: &str) -> Option<Vec<u8>> {
    std::env::var_os(name).map(|v| v.as_bytes().to_vec())
}

/// parse ~/ and %/ in filename and returns the translated filename
pub fn parse_filename(filename: &[u8], buf_dir: &[u8]) -> Vec<u8> {
    let prefix = &filename[..filename.len().min(2)];
    if prefix == b"~" || prefix == b"~/" {
        let mut res = homedir();
        res.extend_from_slice(&filename[1..]);
        return res;
    }
    if (prefix == b"%" || prefix == b"%/") && !buf_dir.is_empty() {
        let mut res = buf_dir.to_vec();
        res.extend_from_slice(&filename[1..]);
        return res;
    }
    filename.to_vec()
}

/// returns (directory, filename)
pub fn split_path(path: &[u8]) -> (&[u8], &[u8]) {
    match path.iter().rposition(|&c| c == b'/') {
        None => (&path[..0], path),
        Some(slash) => (&path[..slash + 1], &path[slash + 1..]),
    }
}

fn realpath(path: &[u8]) -> Option<Vec<u8>> {
    let c = cstr(path);
    let mut buffer = vec![0u8; libc::PATH_MAX as usize + 1];
    let res = unsafe { libc::realpath(c.as_ptr(), buffer.as_mut_ptr() as *mut libc::c_char) };
    if res.is_null() {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    buffer.truncate(len);
    Some(buffer)
}

pub fn getcwd() -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 1024];
    let res = unsafe { libc::getcwd(buf.as_mut_ptr() as *mut libc::c_char, 1024) };
    if res.is_null() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    buf.truncate(len);
    Some(buf)
}

pub fn real_path(filename: &[u8]) -> Vec<u8> {
    if filename.is_empty() {
        return Vec::new();
    }
    let mut existing: &[u8] = filename;
    let mut non_existing: &[u8] = &[];
    loop {
        if let Some(res) = realpath(existing) {
            if non_existing.is_empty() {
                return res;
            }
            let mut dir: &[u8] = &res;
            while dir.last() == Some(&b'/') {
                dir = &dir[..dir.len() - 1];
            }
            return kformat!("{}/{}", dir, non_existing);
        }
        // find '/' starting from the character before last
        let search_end = existing.len().saturating_sub(1);
        match existing[..search_end].iter().rposition(|&c| c == b'/') {
            None => {
                let cwd = getcwd().unwrap_or_default();
                return kformat!("{}/{}", cwd, filename);
            }
            Some(pos) => {
                existing = &filename[..pos + 1];
                non_existing = &filename[pos + 1..];
            }
        }
    }
}

pub fn compact_path(filename: &[u8]) -> Result<Vec<u8>> {
    let real_filename = real_path(filename);
    let Some(cwd) = getcwd() else {
        return Err(rt_err!(
            "unable to get the current working directory (errno: {})",
            last_error_str()
        ));
    };
    let mut real_cwd = real_path(&cwd);
    real_cwd.push(b'/');
    if prefix_match(&real_filename, &real_cwd) {
        return Ok(real_filename[real_cwd.len()..].to_vec());
    }
    let home_owned = homedir();
    let mut home: &[u8] = &home_owned;
    while home.last() == Some(&b'/') {
        home = &home[..home.len() - 1];
    }
    if !home.is_empty() && real_filename.starts_with(home) {
        let mut res = b"~".to_vec();
        res.extend_from_slice(&real_filename[home.len()..]);
        return Ok(res);
    }
    Ok(filename.to_vec())
}

pub fn tmpdir() -> Vec<u8> {
    match getenv("TMPDIR") {
        Some(t) if !t.is_empty() => {
            if t.last() == Some(&b'/') {
                t[..t.len() - 1].to_vec()
            } else {
                t
            }
        }
        _ => b"/tmp".to_vec(),
    }
}

pub fn homedir() -> Vec<u8> {
    match getenv("HOME") {
        Some(h) if !h.is_empty() => h,
        _ => unsafe {
            let pw = libc::getpwuid(libc::geteuid());
            if pw.is_null() {
                return Vec::new();
            }
            CStr::from_ptr((*pw).pw_dir).to_bytes().to_vec()
        },
    }
}

fn select_one(fd: i32, write: bool) -> bool {
    unsafe {
        let mut fds: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut fds);
        libc::FD_SET(fd, &mut fds);
        let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
        let (r, w) = if write {
            (std::ptr::null_mut(), &mut fds as *mut _)
        } else {
            (&mut fds as *mut _, std::ptr::null_mut())
        };
        libc::select(fd + 1, r, w, std::ptr::null_mut(), &mut tv) == 1
    }
}

pub fn fd_readable(fd: i32) -> bool {
    select_one(fd, false)
}

pub fn fd_writable(fd: i32) -> bool {
    select_one(fd, true)
}

pub fn read_fd(fd: i32, text: bool) -> Result<Vec<u8>> {
    let mut content = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let size = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if size == 0 {
            break;
        }
        if size == -1 {
            return Err(fd_access_error(fd, &last_error_str()));
        }
        let data = &buf[..size as usize];
        if text {
            content.extend(data.iter().filter(|&&c| c != b'\r'));
        } else {
            content.extend_from_slice(data);
        }
    }
    Ok(content)
}

pub fn read_file(filename: &[u8], text: bool) -> Result<Vec<u8>> {
    let c = cstr(filename);
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY) };
    if fd == -1 {
        return Err(file_access_error(filename, &last_error_str()));
    }
    let res = read_fd(fd, text);
    unsafe { libc::close(fd) };
    res
}

/// Port of MappedFile: reads the whole content of a file
pub struct MappedFile {
    pub data: Vec<u8>,
    pub st: libc::stat,
}

impl MappedFile {
    pub fn new(filename: &[u8]) -> Result<MappedFile> {
        let c = cstr(filename);
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        if fd == -1 {
            return Err(file_access_error(filename, &last_error_str()));
        }
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        unsafe { libc::fstat(fd, &mut st) };
        if (st.st_mode & libc::S_IFMT) == libc::S_IFDIR {
            unsafe { libc::close(fd) };
            return Err(file_access_error(filename, b"is a directory"));
        }
        let mut data = Vec::with_capacity(st.st_size.max(0) as usize);
        if st.st_size > 0 {
            if st.st_size > i32::MAX as i64 {
                unsafe { libc::close(fd) };
                return Err(Error::runtime("file is too big"));
            }
            let mut buf = vec![0u8; 65536];
            while (data.len() as i64) < st.st_size {
                let size = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if size == 0 {
                    break;
                }
                if size == -1 {
                    let err = errno();
                    if err == libc::EINTR || err == libc::EAGAIN {
                        continue;
                    }
                    unsafe { libc::close(fd) };
                    return Err(file_access_error(filename, &strerror(err)));
                }
                data.extend_from_slice(&buf[..size as usize]);
            }
        }
        unsafe { libc::close(fd) };
        Ok(MappedFile { data, st })
    }
}

fn stat(filename: &[u8]) -> Option<libc::stat> {
    let c = cstr(filename);
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(c.as_ptr(), &mut st) } == 0 { Some(st) } else { None }
}

pub fn is_dir_mode(mode: libc::mode_t) -> bool {
    (mode & libc::S_IFMT) == libc::S_IFDIR
}

pub fn is_reg_mode(mode: libc::mode_t) -> bool {
    (mode & libc::S_IFMT) == libc::S_IFREG
}

pub fn file_exists(filename: &[u8]) -> bool {
    stat(filename).is_some()
}

pub fn regular_file_exists(filename: &[u8]) -> bool {
    stat(filename).is_some_and(|st| is_reg_mode(st.st_mode))
}

/// write all data to fd. When not atomic, uses non blocking writes and
/// processes urgent events while waiting.
pub fn write_fd(fd: i32, data: &[u8], atomic: bool) -> Result<()> {
    let has_event_manager = crate::event_manager::EventManager::has_instance();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if !atomic && has_event_manager {
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    }
    let mut pos = 0;
    let mut result = Ok(());
    while pos < data.len() {
        let written =
            unsafe { libc::write(fd, data[pos..].as_ptr() as *const libc::c_void, data.len() - pos) };
        if written != -1 {
            pos += written as usize;
        } else {
            let err = errno();
            if err == libc::EAGAIN && !atomic && has_event_manager {
                crate::event_manager::EventManager::instance().handle_next_events(
                    crate::event_manager::EventMode::Urgent,
                    None,
                    Some(std::time::Duration::ZERO),
                );
            } else {
                result = Err(fd_access_error(fd, &strerror(err)));
                break;
            }
        }
    }
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    result
}

pub fn write(fd: i32, data: &[u8]) -> Result<()> {
    write_fd(fd, data, false)
}

pub fn create_file(filename: &[u8]) -> i32 {
    let has_event_manager = crate::event_manager::EventManager::has_instance();
    let flags =
        libc::O_CREAT | libc::O_WRONLY | libc::O_TRUNC | if has_event_manager { libc::O_NONBLOCK } else { 0 };
    let c = cstr(filename);
    loop {
        let fd = unsafe { libc::open(c.as_ptr(), flags, 0o644 as libc::c_uint) };
        if fd != -1 {
            return fd;
        }
        if errno() == libc::ENXIO && has_event_manager {
            // trying to open a FIFO with no readers yet
            crate::event_manager::EventManager::instance().handle_next_events(
                crate::event_manager::EventMode::Urgent,
                None,
                Some(std::time::Duration::from_millis(1)),
            );
        } else {
            return -1;
        }
    }
}

pub fn write_to_file(filename: &[u8], data: &[u8]) -> Result<()> {
    let fd = create_file(filename);
    if fd == -1 {
        return Err(file_access_error(filename, &last_error_str()));
    }
    let res = write(fd, data);
    unsafe { libc::close(fd) };
    res
}

/// returns (fd, path)
pub fn open_temp_file(filename: &[u8]) -> (i32, Vec<u8>) {
    let path = real_path(filename);
    let (dir, file) = split_path(&path);
    let mut buffer = if dir.is_empty() {
        kformat!(".{}.kak.XXXXXX", file)
    } else {
        kformat!("{}/.{}.kak.XXXXXX", dir, file)
    };
    buffer.push(0);
    let fd = unsafe { libc::mkstemp(buffer.as_mut_ptr() as *mut libc::c_char) };
    buffer.pop();
    (fd, buffer)
}

pub fn find_file(filename: &[u8], buf_dir: &[u8], paths: &[Vec<u8>]) -> Vec<u8> {
    if filename.starts_with(b"/") {
        if stat(filename).is_some_and(|st| is_reg_mode(st.st_mode)) {
            return filename.to_vec();
        }
        return Vec::new();
    }
    if filename.starts_with(b"~/") {
        let mut candidate = homedir();
        candidate.extend_from_slice(&filename[1..]);
        if stat(&candidate).is_some_and(|st| is_reg_mode(st.st_mode)) {
            return candidate;
        }
        return Vec::new();
    }
    for path in paths {
        let mut candidate = parse_filename(path, buf_dir);
        if !candidate.is_empty() && candidate.last() != Some(&b'/') {
            candidate.push(b'/');
        }
        candidate.extend_from_slice(filename);
        if stat(&candidate).is_some_and(|st| is_reg_mode(st.st_mode)) {
            return candidate;
        }
    }
    Vec::new()
}

pub fn make_directory(dir: &[u8], mode: libc::mode_t) -> Result<()> {
    let end = dir.len();
    let mut it = 0;
    while it != end {
        it = dir[(it + 1).min(end)..].iter().position(|&c| c == b'/').map(|p| p + it + 1).unwrap_or(end);
        let dirname = &dir[..it];
        match stat(dirname) {
            Some(st) => {
                if !is_dir_mode(st.st_mode) {
                    return Err(rt_err!("cannot make directory, '{}' exists but is not a directory", dirname));
                }
            }
            None => {
                let old_mask = unsafe { libc::umask(0) };
                let c = cstr(dirname);
                let res = unsafe { libc::mkdir(c.as_ptr(), mode) };
                unsafe { libc::umask(old_mask) };
                if res != 0 {
                    return Err(rt_err!("mkdir failed for directory '{}' errno {}", dirname, errno()));
                }
            }
        }
    }
    Ok(())
}

pub fn list_files(dirname: &[u8], callback: &mut dyn FnMut(&[u8], &libc::stat)) {
    let path = if dirname.is_empty() { b"./".to_vec() } else { dirname.to_vec() };
    let c = cstr(&path);
    let dir = unsafe { libc::opendir(c.as_ptr()) };
    if dir.is_null() {
        return;
    }
    loop {
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()).to_bytes().to_vec() };
        if name.is_empty() {
            continue;
        }
        let full = if dirname.is_empty() || dirname.last() == Some(&b'/') {
            kformat!("{}{}", dirname, name)
        } else {
            kformat!("{}/{}", dirname, name)
        };
        let Some(st) = stat(&full) else { continue };
        let filename = if is_dir_mode(st.st_mode) { kformat!("{}/", name) } else { name };
        callback(&filename, &st);
    }
    unsafe { libc::closedir(dir) };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

pub const INVALID_TIME: Timespec = Timespec { tv_sec: -1, tv_nsec: -1 };

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FsStatus {
    pub timestamp: Timespec,
    pub file_size: i64,
    pub hash: usize,
}

impl Default for FsStatus {
    fn default() -> Self {
        FsStatus { timestamp: INVALID_TIME, file_size: 0, hash: 0 }
    }
}

pub fn st_mtim(st: &libc::stat) -> Timespec {
    #[cfg(target_os = "macos")]
    {
        Timespec { tv_sec: st.st_mtime, tv_nsec: st.st_mtime_nsec }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Timespec { tv_sec: st.st_mtime as i64, tv_nsec: st.st_mtime_nsec as i64 }
    }
}

pub fn get_fs_timestamp(filename: &[u8]) -> Timespec {
    match stat(filename) {
        Some(st) => st_mtim(&st),
        None => INVALID_TIME,
    }
}

pub fn get_fs_status(filename: &[u8]) -> Result<FsStatus> {
    let file = MappedFile::new(filename)?;
    Ok(FsStatus { timestamp: st_mtim(&file.st), file_size: file.st.st_size, hash: murmur3(&file.data) })
}

pub fn get_kak_binary_path() -> Result<Vec<u8>> {
    match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(p) => Ok(p.as_os_str().as_bytes().to_vec()),
        Err(_) => Err(Error::runtime("unable to get the executable path")),
    }
}

/// Port of BufferedWriter
pub struct BufferedWriter {
    fd: i32,
    atomic: bool,
    buffer: Vec<u8>,
}

impl BufferedWriter {
    const SIZE: usize = 4096;
    pub fn new(fd: i32, atomic: bool) -> Self {
        BufferedWriter { fd, atomic, buffer: Vec::with_capacity(Self::SIZE) }
    }
    pub fn write(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            let write_len = data.len().min(Self::SIZE - self.buffer.len());
            self.buffer.extend_from_slice(&data[..write_len]);
            if self.buffer.len() == Self::SIZE {
                self.flush()?;
            }
            data = &data[write_len..];
        }
        Ok(())
    }
    pub fn flush(&mut self) -> Result<()> {
        let res = write_fd(self.fd, &self.buffer, self.atomic);
        self.buffer.clear();
        res
    }
}

// based on https://github.com/PeterScott/murmur3
pub fn murmur3(data: &[u8]) -> usize {
    let len = data.len();
    let mut hash: u32 = 0x1235678;
    const C1: u32 = 0xcc9e2d51;
    const C2: u32 = 0x1b873593;
    let nblocks = len / 4;
    for i in 0..nblocks {
        let b = &data[i * 4..i * 4 + 4];
        let mut key = (b[3] as u32) << 24 | (b[2] as u32) << 16 | (b[1] as u32) << 8 | b[0] as u32;
        key = key.wrapping_mul(C1);
        key = key.rotate_left(15);
        key = key.wrapping_mul(C2);
        hash ^= key;
        hash = hash.rotate_left(13);
        hash = hash.wrapping_mul(5).wrapping_add(0xe6546b64);
    }
    let tail = &data[nblocks * 4..];
    let mut key: u32 = 0;
    let rem = len & 3;
    if rem == 3 {
        key ^= (tail[2] as u32) << 16;
    }
    if rem >= 2 {
        key ^= (tail[1] as u32) << 8;
    }
    if rem >= 1 {
        key ^= tail[0] as u32;
        key = key.wrapping_mul(C1);
        key = key.rotate_left(15);
        key = key.wrapping_mul(C2);
        hash ^= key;
    }
    hash ^= len as u32;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85ebca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2ae35);
    hash ^= hash >> 16;
    hash as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_murmur() {
        assert_eq!(murmur3(b"Hello, World!"), 0xf816f95b);
    }
    #[test]
    fn test_split_path() {
        assert_eq!(split_path(b"foo/bar"), (&b"foo/"[..], &b"bar"[..]));
        assert_eq!(split_path(b"bar"), (&b""[..], &b"bar"[..]));
    }
}
