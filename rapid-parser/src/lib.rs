//! Parses RFC 5322 / MIME mail into NDJSON records, to be indexed.

use std::borrow::Cow;
use std::io::{self, Write};

use mail_parser::{AddressList, Message};
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
        body_text: msg.body_text(0),
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

/// Writes one JSON object per line
pub fn write_ndjson<'a>(
    records: impl IntoIterator<Item = Record<'a>>,
    mut out: impl Write,
) -> io::Result<()> {
    for r in records {
        serde_json::to_writer(&mut out, &r)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}
