//! Parses RFC 5322 / MIME mail into NDJSON records, to be indexed.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

use mail_parser::decoders::html::html_to_text;
use mail_parser::mailbox::maildir::{Flag, FolderIterator};
use mail_parser::mailbox::mbox::MessageIterator;
use mail_parser::{AddressList, Message, MessageBuffers, MessageParser, PartKind};
use serde::Serialize;

/// One message per Record
#[derive(Debug, Serialize)]
pub struct Record<'a> {
    pub message_id: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub from: Vec<Address<'a>>,
    pub to: Vec<Address<'a>>,
    pub cc: Vec<Address<'a>>,
    /// RFC 3339
    pub date: Option<String>,
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
        date: msg.date().map(|d| d.to_rfc3339()),
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
    let parser = MessageParser::default();
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

/// Calls `f` with the record of every message in an mbox file; mail-parser uses
/// mboxrd unquoting
pub fn read_mbox(
    path: impl AsRef<Path>,
    mut f: impl FnMut(Record<'_>) -> io::Result<()>,
) -> io::Result<()> {
    let path = path.as_ref();
    let parser = MessageParser::default();
    let mut buffers = MessageBuffers::new();
    for (i, entry) in MessageIterator::new(BufReader::new(File::open(path)?)).enumerate() {
        let entry = entry?;
        let msg = parse(&parser, entry.contents(), &mut buffers, || {
            format!("{} (message {})", path.display(), i + 1)
        })?;
        f(record(&msg))?;
        buffers = msg.into_buffers();
    }
    Ok(())
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
