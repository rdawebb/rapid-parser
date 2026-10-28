//! Parses RFC 5322 / MIME mail into a Record struct, and optionally NDJSON

use std::borrow::Cow;
use std::fs::File;
use std::io;
use std::path::Path;

use mail_parser::decoders::html::html_to_text;
use mail_parser::mailbox::maildir::{Flag, FolderIterator};
use mail_parser::{
    AddressList, DateTime, HeaderForm, HeaderName, Message, MessageBuffers, MessageParser, PartKind,
};
use memchr::{memchr, memmem};
use memmap2::Mmap;
use serde::Serialize;

/// One message per Record
#[derive(Debug, Serialize)]
pub struct Record<'a> {
    pub message_id: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub from: Vec<Address<'a>>,
    pub to: Vec<Address<'a>>,
    pub cc: Vec<Address<'a>>,

    /// Serialised as RFC 3339
    #[serde(serialize_with = "rfc3339")]
    pub date: Option<DateTime>,
    pub in_reply_to: Vec<&'a str>,
    pub references: Vec<&'a str>,
    pub body_text: Option<Cow<'a, str>>,
    pub attachments: Vec<AttachmentMetadata<'a>>,
}

#[derive(Debug, Serialize)]
pub struct Address<'a> {
    pub name: Option<&'a str>,
    pub address: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct AttachmentMetadata<'a> {
    pub name: Option<&'a str>,
    pub content_type: Option<String>,
    /// Decoded size in bytes, computed without decoding
    pub size: usize,
    pub inline: bool,
}

/// Extracts the record of a parsed message; borrows from the Message
pub fn record<'a>(msg: &'a Message<'_>) -> Record<'a> {
    let addrs = |list: Option<AddressList<'a>>| {
        list.into_iter()
            .flat_map(|l| l.mailboxes())
            .map(|m| Address {
                name: m.name(),
                address: m.address(),
            })
            .collect()
    };

    Record {
        message_id: msg.message_id(),
        subject: msg.subject(),
        from: addrs(msg.from()),
        to: addrs(msg.to()),
        cc: addrs(msg.cc()),
        date: msg.date(),
        in_reply_to: msg
            .in_reply_to()
            .map_or_else(Vec::new, |l| l.iter().collect()),
        references: msg
            .references()
            .map_or_else(Vec::new, |l| l.iter().collect()),
        body_text: body_text(msg),
        attachments: msg
            .attachments()
            .map(|p| AttachmentMetadata {
                name: p.attachment_name(),
                content_type: p.content_type().map(|ct| match ct.subtype() {
                    Some(sub) => format!("{}/{sub}", ct.ctype()),
                    None => ct.ctype().to_owned(),
                }),
                size: p.decoded_len(),
                inline: p.is_inline(),
            })
            .collect(),
    }
}

/// `DateTime::to_rfc3339` without `format!` and allocation
fn rfc3339<S: serde::Serializer>(date: &Option<DateTime>, s: S) -> Result<S::Ok, S::Error> {
    let Some(d) = date else {
        return s.serialize_none();
    };
    // Too wide for the fixed layout
    if d.year > 9999 {
        return s.serialize_some(&d.to_rfc3339());
    }

    let mut b = *b"0000-00-00T00:00:00+00:00";
    for (i, p) in [1000, 100, 10, 1].into_iter().enumerate() {
        b[i] = b'0' + (d.year / p % 10) as u8;
    }
    for (at, n) in [
        (5, d.month),
        (8, d.day),
        (11, d.hour),
        (14, d.minute),
        (17, d.second),
        (20, d.tz_hour),
        (23, d.tz_minute),
    ] {
        if n > 99 {
            return s.serialize_some(&d.to_rfc3339());
        }
        b[at] = b'0' + n / 10;
        b[at + 1] = b'0' + n % 10;
    }
    let len = if d.tz_hour == 0 && d.tz_minute == 0 {
        b[19] = b'Z';
        20
    } else {
        if d.tz_before_gmt {
            b[19] = b'-';
        }
        25
    };
    s.serialize_some(std::str::from_utf8(&b[..len]).expect("ASCII"))
}

/// The first text body part, CRLF as LF; mail-parser's `html_to_text` breaks
/// lines only at `<br>` and `</p>`, so other block tags get a `<br>` first
fn body_text<'a>(msg: &'a Message<'_>) -> Option<Cow<'a, str>> {
    let part = msg.text_body().next()?;
    let text = part.text()?;

    match part.kind() {
        PartKind::Html => {
            let plain = html_to_text(&break_blocks(&text));
            let mut out = String::with_capacity(plain.len());
            for line in plain.lines().filter(|l| !l.trim().is_empty()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(line);
            }
            Some(Cow::Owned(out))
        }
        PartKind::Text if text.contains('\r') => Some(Cow::Owned(text.replace("\r\n", "\n"))),
        PartKind::Text => Some(text),
        _ => None,
    }
}

const BLOCK_TAGS: [&str; 14] = [
    "p",
    "div",
    "br",
    "li",
    "tr",
    "td",
    "th",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
];

/// Puts a `<br>` before every opening or closing block tag
fn break_blocks(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + html.len() / 8);
    let mut rest = html;

    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        let tag = rest.strip_prefix('/').unwrap_or(rest);
        let name = &tag[..tag
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(tag.len())];
        if BLOCK_TAGS.iter().any(|t| t.eq_ignore_ascii_case(name)) {
            out.push_str("<br>");
        }
        out.push('<');
    }
    out.push_str(rest);
    out
}

/// A Maildir message: its record plus location and flags
#[derive(Debug, Serialize)]
pub struct MaildirRecord<'a> {
    /// Maildir++ folder name, `INBOX` for the root
    pub folder: &'a str,
    pub flags: Vec<&'static str>,
    #[serde(flatten)]
    pub record: Record<'a>,
}

/// Calls `f` with the record of every message in a Maildir++ tree (`cur/` and
/// `new/` of the root and of every `.`-prefixed subfolder); records borrow
/// parse buffers that are reused for the next message
pub fn read_maildir(
    root: impl AsRef<Path>,
    mut f: impl FnMut(MaildirRecord<'_>) -> io::Result<()>,
) -> io::Result<()> {
    let parser = MessageParser::new().header(HeaderName::Received, HeaderForm::Raw);
    let mut buffers = MessageBuffers::new();

    for folder in FolderIterator::new(root.as_ref(), Some("."))? {
        let folder = folder?;
        let name = folder.name().unwrap_or("INBOX").to_owned();
        for entry in folder {
            let entry = entry?;
            let msg = parse(&parser, entry.contents(), &mut buffers, || {
                entry.path().display().to_string()
            })?;

            f(MaildirRecord {
                folder: &name,
                flags: entry.flags().iter().map(|&fl| flag_name(fl)).collect(),
                record: record(&msg),
            })?;
            buffers = msg.into_buffers();
        }
    }
    Ok(())
}

/// Calls `f` with the record of every message in an mbox file, with mboxrd
/// unquoting; the file is mapped, so shouldn't be truncated or rewritten
/// while this runs; appending to it is fine
pub fn read_mbox(
    path: impl AsRef<Path>,
    mut f: impl FnMut(Record<'_>) -> io::Result<()>,
) -> io::Result<()> {
    let path = path.as_ref();
    let file = File::open(path)?;

    // Mapping an empty file fails on Windows
    if file.metadata()?.len() == 0 {
        return Ok(());
    }

    // SAFETY: the caller doesn't truncate or rewrite the file
    let map = unsafe { Mmap::map(&file)? };

    // Read-ahead; only a hint, so a failure is ignored
    #[cfg(unix)]
    let _ = map.advise(memmap2::Advice::WillNeed);
    let parser = MessageParser::new().header(HeaderName::Received, HeaderForm::Raw);
    let mut buffers = MessageBuffers::new();

    for (i, raw) in mbox_messages(&map).enumerate() {
        let msg = parse(&parser, &raw, &mut buffers, || {
            format!("{} (message {})", path.display(), i + 1)
        })?;
        f(record(&msg))?;
        buffers = msg.into_buffers();
    }
    Ok(())
}

/// The messages of an mbox buffer, without their `From ` line or the blank
/// line before the next one; each borrows the buffer, unless it has a `>From `
/// line to unquote; anything before the first `From ` line is skipped
fn mbox_messages(buf: &[u8]) -> impl Iterator<Item = Cow<'_, [u8]>> {
    let mut rest = match buf.starts_with(b"From ") {
        true => buf,
        false => memmem::find(buf, b"\nFrom ").map_or(&[][..], |i| &buf[i + 1..]),
    };
    let from = memmem::Finder::new(b"From ");
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }

        // Scan finds the next `From ` line (at `body`, if this message is
        // empty) and any `>From ` lines to unquote
        let body = memchr(b'\n', rest).map_or(rest.len(), |i| i + 1);
        let (mut next, mut quoted) = (rest.len(), false);

        for hit in from.find_iter(&rest[body..]).map(|i| body + i) {
            match rest[hit - 1] {
                b'\n' => {
                    next = hit;
                    break;
                }
                // rest[body - 1] is a \n, so a run of '>' always stops at one
                b'>' if !quoted => {
                    let before = rest[..hit].iter().rposition(|&b| b != b'>');
                    quoted = before.is_some_and(|i| rest[i] == b'\n');
                }
                _ => {}
            }
        }
        let mut msg = &rest[body..next];
        rest = &rest[next..];
        if msg.ends_with(b"\r\n\r\n") {
            msg = &msg[..msg.len() - 2];
        } else if msg.ends_with(b"\n\n") {
            msg = &msg[..msg.len() - 1];
        }

        Some(match quoted {
            true => Cow::Owned(unquote(msg)),
            false => Cow::Borrowed(msg),
        })
    })
}

/// mboxrd: a line of `>From `, `>>From `, ... loses one `>`
fn unquote(msg: &[u8]) -> Vec<u8> {
    let (mut out, mut copied) = (Vec::new(), 0);

    for hit in memmem::find_iter(msg, b">From ") {
        let before = msg[..hit].iter().rposition(|&b| b != b'>');
        if before.is_none_or(|i| msg[i] == b'\n') {
            out.extend_from_slice(&msg[copied..hit]);
            copied = hit + 1;
        }
    }
    // Only called once mbox_messages has seen a line to unquote
    debug_assert!(copied > 0);
    out.extend_from_slice(&msg[copied..]);
    out
}

/// mail-parser only fails on inputs of 4 GiB or more; names the message
fn parse<'x>(
    parser: &MessageParser,
    raw: &'x [u8],
    buffers: &mut MessageBuffers,
    what: impl FnOnce() -> String,
) -> io::Result<Message<'x>> {
    parser.parse_with(raw, buffers).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unparseable message: {}", what()),
        )
    })
}

fn flag_name(flag: Flag) -> &'static str {
    match flag {
        Flag::Passed => "passed",
        Flag::Replied => "replied",
        Flag::Seen => "seen",
        Flag::Trashed => "trashed",
        Flag::Draft => "draft",
        Flag::Flagged => "flagged",
    }
}

/// Writes one JSON object per line
#[cfg(feature = "ndjson")]
pub fn write_ndjson<'a>(
    records: impl IntoIterator<Item = Record<'a>>,
    mut out: impl io::Write,
) -> io::Result<()> {
    for r in records {
        serde_json::to_writer(&mut out, &r)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use mail_parser::DateTime;

    #[test]
    fn rfc3339_matches_mail_parser() {
        #[derive(serde::Serialize)]
        struct D(#[serde(serialize_with = "super::rfc3339")] Option<DateTime>);

        let d = |year, tz_before_gmt, tz_hour, tz_minute| DateTime {
            year,
            month: 3,
            day: 9,
            hour: 7,
            minute: 5,
            second: 59,
            tz_before_gmt,
            tz_hour,
            tz_minute,
        };
        for date in [
            d(2026, false, 0, 0),
            d(2026, true, 0, 0),
            d(987, true, 5, 30),
            d(2019, false, 1, 0),
            d(12345, false, 0, 0),
            d(2026, true, 123, 0),
        ] {
            let json = serde_json::to_string(&D(Some(date))).unwrap();
            assert_eq!(json, format!("\"{}\"", date.to_rfc3339()));
        }
        assert_eq!(serde_json::to_string(&D(None)).unwrap(), "null");
    }

    #[test]
    fn mbox_split_and_unquote() {
        // Preamble, a quoted line, an empty message, a mid-line `>From `
        let buf = b"junk\nFrom a\n>From x\nFrom b\nFrom c\nsee a>From y\n";
        let msgs: Vec<_> = super::mbox_messages(buf).collect();
        assert_eq!(msgs, [&b"From x\n"[..], b"", b"see a>From y\n"]);
    }
}
