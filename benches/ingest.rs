//! Ingestion throughput over the corpora in `datasets/` (see its README).
//! `cargo bench --bench ingest [filter]`; skips any corpus that is absent.

use std::cell::OnceCell;
use std::fs::{self, File};
use std::hint::black_box;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mail_parser::mailbox::mbox::MessageIterator;
use mail_parser::{HeaderForm, HeaderName, Message, MessageBuffers, MessageParser};
use memmap2::Mmap;
use rapid_parser::{record, write_ndjson};

const RUNS: usize = 3;

fn main() -> io::Result<()> {
    let filter = std::env::args().skip(1).find(|a| !a.starts_with("--"));
    let datasets = Path::new(env!("CARGO_MANIFEST_DIR")).join("datasets");
    let run =
        |name: &str, runs: usize, f: &dyn Fn() -> io::Result<(usize, usize)>| -> io::Result<()> {
            if filter.as_deref().is_some_and(|p| !name.contains(p)) {
                return Ok(());
            }
            let (mut best, mut counts) = (Duration::MAX, (0, 0));
            for _ in 0..runs {
                let t = Instant::now();
                counts = f()?;
                best = best.min(t.elapsed());
            }
            let (n, bytes) = counts;
            let s = best.as_secs_f64();
            println!(
                "{name:<34} {n:>7} msgs {:>9.0} msg/s {:>7.1} MB/s",
                n as f64 / s,
                bytes as f64 / s / 1e6
            );
            Ok(())
        };

    // One file per message: the Enron tree is plain folders of files, so it's
    // walked directly
    let enron = datasets.join("maildir");
    // A filter naming one corpus skips the other entirely
    let only = |corpus: &str| filter.as_deref().is_some_and(|p| p.starts_with(corpus));
    if enron.is_dir() && !only("mbox") {
        let mut paths = Vec::new();
        walk(&enron, &mut paths)?;
        paths.sort();
        let parser = MessageParser::default();
        let with = parsing(&parser);
        // One open per message makes these slow, so they run fewer times; the
        // first run of "enron read" warms the page cache
        run("enron read", 2, &|| {
            let mut bytes = 0;
            for p in &paths {
                bytes += black_box(fs::read(p)?).len();
            }
            Ok((paths.len(), bytes))
        })?;
        run("enron read+parse_with", 1, &|| {
            let mut buffers = MessageBuffers::new();
            let mut bytes = 0;
            for p in &paths {
                let raw = fs::read(p)?;
                bytes += raw.len();
                with(&raw, &mut buffers);
            }
            Ok((paths.len(), bytes))
        })?;
        run("enron mmap+parse_with", 1, &|| {
            let mut buffers = MessageBuffers::new();
            let mut bytes = 0;
            for p in &paths {
                let file = File::open(p)?;
                if file.metadata()?.len() == 0 {
                    continue; // Mapping an empty file fails on Windows
                }
                // SAFETY: the corpus isn't modified while the bench runs
                let map = unsafe { Mmap::map(&file)? };
                bytes += map.len();
                with(&map, &mut buffers);
            }
            Ok((paths.len(), bytes))
        })?;

        // Parser cases from memory, so file I/O doesn't hide them; loaded on
        // first use, so a filter that skips them skips the load
        let raws = OnceCell::new();
        let mem = |f: &dyn Fn(&[u8], &mut MessageBuffers)| -> io::Result<(usize, usize)> {
            let raws: &Vec<Vec<u8>> = match raws.get() {
                Some(r) => r,
                None => {
                    let loaded = paths.iter().map(fs::read).collect::<io::Result<_>>()?;
                    raws.get_or_init(|| loaded)
                }
            };
            let mut buffers = MessageBuffers::new();
            for raw in raws {
                f(raw, &mut buffers);
            }
            Ok((raws.len(), raws.iter().map(Vec::len).sum()))
        };
        run("enron mem parse", RUNS, &|| {
            mem(&|raw, _| _ = black_box(parser.parse(raw)))
        })?;
        run("enron mem parse_with", RUNS, &|| mem(&with))?;
        run("enron mem parse_with+record", RUNS, &|| {
            mem(&|raw, b| parse_with(&parser, raw, b, |m| _ = black_box(record(m))))
        })?;
        run("enron mem parse_with+ndjson", RUNS, &|| {
            mem(&|raw, b| {
                parse_with(&parser, raw, b, |m| {
                    write_ndjson([record(m)], io::sink()).unwrap()
                })
            })
        })?;
        run("enron mem mailparse", RUNS, &|| {
            mem(&|raw, _| _ = black_box(mailparse::parse_mail(raw)))
        })?;
    } else if !enron.is_dir() {
        println!("skip enron: no {}", enron.display());
    }

    let mbox = datasets.join("httpd-dev.mbox");
    if mbox.is_file() && !only("enron") {
        let parser = MessageParser::default();
        // Received is the costliest header and Record doesn't read it
        let raw_received = MessageParser::new().header(HeaderName::Received, HeaderForm::Raw);
        let split = |src: &mut dyn io::BufRead,
                     f: &dyn Fn(&[u8], &mut MessageBuffers)|
         -> io::Result<(usize, usize)> {
            let mut buffers = MessageBuffers::new();
            let (mut n, mut bytes) = (0, 0);
            for entry in MessageIterator::new(src) {
                let entry = entry?;
                n += 1;
                bytes += entry.contents().len();
                f(entry.contents(), &mut buffers);
            }
            Ok((n, bytes))
        };
        // SAFETY: the corpus isn't modified while the bench runs
        let map_mbox = |_willneed: bool| -> io::Result<Mmap> {
            let map = unsafe { Mmap::map(&File::open(&mbox)?)? };
            #[cfg(unix)]
            if _willneed {
                map.advise(memmap2::Advice::WillNeed)?;
            }
            Ok(map)
        };
        // Reused across runs, so its pages are already faulted in: for the
        // split cases only, which time the scan and copy
        let map = map_mbox(false)?;
        run("mbox split, no copy", RUNS, &|| {
            let n = usize::from(map.starts_with(b"From "))
                + memchr::memmem::find_iter(&map, b"\nFrom ").count();
            Ok((n, map.len()))
        })?;
        run("mbox split+copy (mmap)", RUNS, &|| {
            split(&mut &map[..], &|_, _| {})
        })?;
        run("mbox bufread+parse_with", RUNS, &|| {
            split(&mut BufReader::new(File::open(&mbox)?), &parsing(&parser))
        })?;
        run("mbox read+parse_with", RUNS, &|| {
            split(&mut &fs::read(&mbox)?[..], &parsing(&parser))
        })?;
        // Mapped afresh each run, as read_mbox does
        run("mbox mmap+parse_with", RUNS, &|| {
            split(&mut &map_mbox(false)?[..], &parsing(&parser))
        })?;
        run("mbox mmap WillNeed+parse_with", RUNS, &|| {
            split(&mut &map_mbox(true)?[..], &parsing(&parser))
        })?;
        run("mbox mmap+parse_with, raw Received", RUNS, &|| {
            split(&mut &map_mbox(true)?[..], &parsing(&raw_received))
        })?;
        run("mbox read_mbox+ndjson", RUNS, &|| {
            let mut n = 0;
            rapid_parser::read_mbox(&mbox, |r| {
                n += 1;
                write_ndjson([r], io::sink())
            })?;
            Ok((n, map.len()))
        })?;
        run("mbox mmap+mailparse", RUNS, &|| {
            split(&mut &map_mbox(true)?[..], &|raw, _| {
                _ = black_box(mailparse::parse_mail(raw))
            })
        })?;
    } else if !mbox.is_file() {
        println!("skip mbox: no {}", mbox.display());
    }
    Ok(())
}

/// Parses `raw` into reused `buffers` and hands the message to `then`
fn parse_with(
    parser: &MessageParser,
    raw: &[u8],
    buffers: &mut MessageBuffers,
    then: impl FnOnce(&Message),
) {
    if let Some(m) = parser.parse_with(raw, buffers) {
        then(&m);
        *buffers = m.into_buffers();
    }
}

/// Parse-only case: the message is kept from being optimised away, then dropped
fn parsing(parser: &MessageParser) -> impl Fn(&[u8], &mut MessageBuffers) + '_ {
    move |raw, b| parse_with(parser, raw, b, |m| _ = black_box(m))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().as_encoded_bytes().starts_with(b".") {
            continue; // .DS_Store, etc.
        } else if entry.file_type()?.is_dir() {
            walk(&entry.path(), out)?;
        } else {
            out.push(entry.path());
        }
    }
    Ok(())
}
