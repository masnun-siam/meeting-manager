//! `recording.partial.wav`: mono 48 kHz 16-bit PCM, appended on its own thread. Every flush
//! interval the header's sizes are patched, so a crash leaves a file that is valid up to the last
//! flush (no `sync_all`: a killed process does not lose the page cache).

use std::fs::OpenOptions;
use std::io::{self, BufWriter, ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use crate::capture::SAMPLE_RATE;

const HEADER_LEN: u64 = 44;
const BYTES_PER_SAMPLE: u64 = 2;

pub enum Chunk {
    Samples(Vec<i16>),
    Close,
}

/// Where the writer writes: the file, or a test's failing sink.
pub trait Sink: Write + Seek + Send {}
impl<T: Write + Seek + Send> Sink for T {}

/// Called once, on the writer thread, when a write fails; the thread then ends.
pub type OnError = Box<dyn FnOnce(ErrorKind) + Send>;

pub struct WavWriter {
    tx: SyncSender<Chunk>,
    thread: JoinHandle<Result<u64, ErrorKind>>,
}

impl WavWriter {
    /// Creates `path` (it must not exist) with mode `0600`, writes the header and starts the
    /// thread. `queue` is the channel's capacity in chunks; `dropped` counts chunks the sender
    /// could not queue, and the writer logs once when it sees one.
    pub fn create(
        path: &Path,
        flush_every: Duration,
        queue: usize,
        dropped: Arc<AtomicU64>,
        on_error: OnError,
    ) -> anyhow::Result<WavWriter> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("cannot create {}", path.display()))?;
        WavWriter::start(Box::new(file), flush_every, queue, dropped, on_error)
            .with_context(|| format!("cannot write {}", path.display()))
    }

    fn start(
        sink: Box<dyn Sink>,
        flush_every: Duration,
        queue: usize,
        dropped: Arc<AtomicU64>,
        on_error: OnError,
    ) -> io::Result<WavWriter> {
        let mut out = BufWriter::with_capacity(64 * 1024, sink);
        out.write_all(&header(0))?;
        out.flush()?;
        let (tx, rx) = mpsc::sync_channel(queue);
        let thread = thread::Builder::new()
            .name("wav-writer".into())
            .spawn(move || {
                let written = write_until_closed(&mut out, &rx, flush_every, &dropped);
                if let Err(kind) = written {
                    on_error(kind);
                }
                written
            })?;
        Ok(WavWriter { tx, thread })
    }

    /// The sender the samples callback queues chunks on (never with a blocking `send`).
    pub fn sender(&self) -> SyncSender<Chunk> {
        self.tx.clone()
    }

    /// Writes what is queued, patches the header a last time and returns the frames written, or
    /// the kind of the write error that ended the thread.
    pub fn finish(self) -> Result<u64, ErrorKind> {
        // Fails only when the thread already ended on an error, which `join` returns.
        let _ = self.tx.send(Chunk::Close);
        drop(self.tx);
        self.thread.join().unwrap_or(Err(ErrorKind::Other))
    }
}

fn write_until_closed(
    out: &mut BufWriter<Box<dyn Sink>>,
    rx: &Receiver<Chunk>,
    flush_every: Duration,
    dropped: &AtomicU64,
) -> Result<u64, ErrorKind> {
    let tick = flush_every.min(Duration::from_secs(1));
    let mut frames = 0u64;
    let mut flushed = Instant::now();
    let mut warned = false;
    loop {
        match rx.recv_timeout(tick) {
            Ok(Chunk::Samples(samples)) => {
                let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                out.write_all(&bytes).map_err(|e| e.kind())?;
                frames += samples.len() as u64;
            }
            Ok(Chunk::Close) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if !warned && dropped.load(Ordering::Relaxed) > 0 {
            warned = true;
            tracing::warn!("recorder: audio dropped because the writer fell behind");
        }
        if flushed.elapsed() >= flush_every {
            patch(out, frames).map_err(|e| e.kind())?;
            flushed = Instant::now();
        }
    }
    patch(out, frames).map_err(|e| e.kind())?;
    Ok(frames)
}

/// Flushes, writes the RIFF and data sizes for `frames`, and returns to the end.
fn patch<W: Write + Seek>(out: &mut W, frames: u64) -> io::Result<()> {
    out.flush()?;
    let data = frames * BYTES_PER_SAMPLE;
    out.seek(SeekFrom::Start(4))?;
    out.write_all(&size(data + HEADER_LEN - 8))?;
    out.seek(SeekFrom::Start(40))?;
    out.write_all(&size(data))?;
    out.flush()?;
    out.seek(SeekFrom::End(0))?;
    Ok(())
}

fn size(n: u64) -> [u8; 4] {
    u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes()
}

/// The canonical 44-byte header for `data` bytes of mono 48 kHz 16-bit PCM.
fn header(data: u64) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&size(data + HEADER_LEN - 8));
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits per sample
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&size(data));
    h
}

/// Patches the sizes of a WAV this writer started, from its length on disk (whole samples
/// only), and returns its frame count. Used after a write error and by crash recovery (#149).
pub fn repair(path: &Path) -> anyhow::Result<u64> {
    let not_ours = || format!("{} is not a recording this app wrote", path.display());
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let len = file.metadata()?.len();
    if len < HEADER_LEN {
        bail!(not_ours());
    }
    let mut head = [0u8; 44];
    file.read_exact(&mut head)?;
    let canonical = header(0);
    if head[..4] != canonical[..4] || head[8..40] != canonical[8..40] {
        bail!(not_ours());
    }
    let frames = (len - HEADER_LEN) / BYTES_PER_SAMPLE;
    patch(&mut file, frames).with_context(|| format!("cannot repair {}", path.display()))?;
    Ok(frames)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    pub(crate) fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn create(path: &Path, flush_every: Duration) -> WavWriter {
        WavWriter::create(
            path,
            flush_every,
            64,
            Arc::new(AtomicU64::new(0)),
            Box::new(|_| {}),
        )
        .unwrap()
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn the_header_is_canonical_mono_48k_16_bit() {
        let mut expected = Vec::new();
        expected.extend_from_slice(b"RIFF");
        expected.extend_from_slice(&36u32.to_le_bytes());
        expected.extend_from_slice(b"WAVEfmt ");
        expected.extend_from_slice(&[16, 0, 0, 0, 1, 0, 1, 0]);
        expected.extend_from_slice(&48_000u32.to_le_bytes());
        expected.extend_from_slice(&96_000u32.to_le_bytes());
        expected.extend_from_slice(&[2, 0, 16, 0]);
        expected.extend_from_slice(b"data");
        expected.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(header(0).to_vec(), expected);

        let dir = temp("wav-header");
        let path = dir.join("a.wav");
        let writer = create(&path, Duration::from_secs(5));
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(writer.finish(), Ok(0));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        let dir = temp("wav-exists");
        let path = dir.join("a.wav");
        fs::write(&path, b"keep").unwrap();
        let error = WavWriter::create(
            &path,
            Duration::from_secs(5),
            4,
            Arc::new(AtomicU64::new(0)),
            Box::new(|_| {}),
        )
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("cannot create"), "{error:#}");
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sizes_are_patched_while_the_writer_is_still_open() {
        let dir = temp("wav-flush");
        let path = dir.join("a.wav");
        let writer = create(&path, Duration::from_millis(20));
        writer
            .sender()
            .send(Chunk::Samples(vec![1, -2, 3]))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let bytes = loop {
            let bytes = fs::read(&path).unwrap();
            if u32_at(&bytes, 40) == 6 || Instant::now() > deadline {
                break bytes;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(u32_at(&bytes, 40), 6);
        assert_eq!(u32_at(&bytes, 4), 42);
        assert_eq!(&bytes[44..], &[1, 0, 0xfe, 0xff, 3, 0]);
        writer.sender().send(Chunk::Samples(vec![4])).unwrap();
        assert_eq!(writer.finish(), Ok(4));
        let bytes = fs::read(&path).unwrap();
        assert_eq!((u32_at(&bytes, 4), u32_at(&bytes, 40)), (44, 8));
        assert_eq!(bytes.len(), 52);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repair_fixes_sizes_from_the_length_and_rejects_foreign_files() {
        let dir = temp("wav-repair");
        let path = dir.join("a.wav");
        let mut bytes = header(0).to_vec();
        bytes.extend_from_slice(&[1, 0, 2, 0, 3, 0, 9]);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(repair(&path).unwrap(), 3);
        let fixed = fs::read(&path).unwrap();
        assert_eq!((u32_at(&fixed, 4), u32_at(&fixed, 40)), (42, 6));
        assert_eq!(fixed.len(), bytes.len());

        let foreign = dir.join("b.wav");
        let mut other = header(0).to_vec();
        other[22] = 2; // stereo
        fs::write(&foreign, &other).unwrap();
        let short = dir.join("c.wav");
        fs::write(&short, [0u8; 10]).unwrap();
        for path in [&foreign, &short] {
            let error = repair(path).unwrap_err().to_string();
            assert!(
                error.contains("is not a recording this app wrote"),
                "{error}"
            );
        }
        assert_eq!(fs::read(&foreign).unwrap(), other);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Accepts `room` bytes, then fails as a full disk does.
    struct Full {
        inner: Cursor<Vec<u8>>,
        room: usize,
    }

    impl Write for Full {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.inner.get_ref().len() + buf.len() > self.room {
                return Err(io::Error::from(ErrorKind::StorageFull));
            }
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for Full {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn a_full_disk_ends_the_writer_with_storage_full() {
        let seen = Arc::new(Mutex::new(None));
        let report = seen.clone();
        let sink = Full {
            inner: Cursor::new(Vec::new()),
            room: 100,
        };
        let writer = WavWriter::start(
            Box::new(sink),
            Duration::from_millis(5),
            4,
            Arc::new(AtomicU64::new(0)),
            Box::new(move |kind| *report.lock().unwrap() = Some(kind)),
        )
        .unwrap();
        // Larger than the writer's buffer, so the write reaches the sink at once.
        let _ = writer.sender().send(Chunk::Samples(vec![0; 40_000]));
        assert_eq!(writer.finish(), Err(ErrorKind::StorageFull));
        assert_eq!(*seen.lock().unwrap(), Some(ErrorKind::StorageFull));
    }
}
