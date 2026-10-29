use rusqlite::{Connection, OpenFlags};

use crate::utils::{cap_chars, collapse_whitespace, decode_text, strip_xml_tags};
use crate::{DocumentParser, ParseStatus, ParsedDocument, ParserInput};

#[derive(Debug, Default)]
pub struct EmailParser;

impl DocumentParser for EmailParser {
    fn name(&self) -> &'static str {
        "eml"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        extension == "eml"
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let Ok(mail) = mailparse::parse_mail(input.bytes) else {
            return ParsedDocument::failed(self.name());
        };
        let mut parts = Vec::new();
        for header in mail.get_headers() {
            let key = header.get_key().to_ascii_lowercase();
            if matches!(
                key.as_str(),
                "subject" | "from" | "to" | "cc" | "bcc" | "date" | "reply-to" | "message-id"
            ) {
                parts.push(format!("{}: {}", header.get_key(), header.get_value()));
            }
        }
        collect_mail_parts(&mail, &mut parts);
        ParsedDocument::new(
            crate::utils::normalize_lines(parts.as_slice(), input.max_chars),
            ParseStatus::Success,
            self.name(),
        )
    }
}

#[derive(Debug, Default)]
pub struct CalendarParser;

impl DocumentParser for CalendarParser {
    fn name(&self) -> &'static str {
        "ics"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        extension == "ics"
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let text = decode_text(input.bytes, input.max_chars.saturating_mul(8));
        let mut parts = Vec::new();
        for line in unfold_ics(text.as_str()) {
            let Some((key_raw, value)) = line.split_once(':') else {
                continue;
            };
            let key = key_raw
                .split(';')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            if is_ics_field(key.as_str()) {
                parts.push(format!("{key}: {}", unescape_ics(value)));
            }
        }
        ParsedDocument::new(
            crate::utils::normalize_lines(parts.as_slice(), input.max_chars),
            ParseStatus::Success,
            self.name(),
        )
    }
}

#[derive(Debug, Default)]
pub struct SqliteParser;

impl DocumentParser for SqliteParser {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        matches!(extension, "sqlite" | "sqlite3" | "db")
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let Ok(connection) = Connection::open_with_flags(input.path, flags) else {
            return ParsedDocument::failed(self.name());
        };
        let mut parts = Vec::new();
        let table_names = table_names(&connection);
        for table in table_names {
            parts.push(format!("Table: {table}"));
            let columns = column_names(&connection, table.as_str());
            if !columns.is_empty() {
                parts.push(format!("Columns: {}", columns.join(", ")));
            }
            append_samples(
                &connection,
                SampleTarget {
                    table: table.as_str(),
                    columns: columns.as_slice(),
                },
                &mut parts,
            );
            if parts.join("\n").len() >= input.max_chars {
                break;
            }
        }
        ParsedDocument::new(
            cap_chars(parts.join("\n").as_str(), input.max_chars),
            ParseStatus::Success,
            self.name(),
        )
    }
}

fn collect_mail_parts(mail: &mailparse::ParsedMail<'_>, parts: &mut Vec<String>) {
    if mail.subparts.is_empty() {
        let mimetype = mail.ctype.mimetype.to_ascii_lowercase();
        if let Ok(body) = mail.get_body() {
            let cleaned = if mimetype == "text/html" {
                strip_xml_tags(body.as_str(), body.len())
            } else {
                body
            };
            if !cleaned.trim().is_empty() {
                parts.push(cleaned);
            }
        }
        return;
    }
    for part in &mail.subparts {
        collect_mail_parts(part, parts);
    }
}

fn unfold_ics(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(previous) = out.last_mut() {
                previous.push_str(line.trim_start());
            }
        } else {
            out.push(line.to_owned());
        }
    }
    out
}

fn unescape_ics(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn is_ics_field(key: &str) -> bool {
    matches!(
        key,
        "SUMMARY"
            | "DTSTART"
            | "DTEND"
            | "DUE"
            | "LOCATION"
            | "DESCRIPTION"
            | "RRULE"
            | "ORGANIZER"
            | "ATTENDEE"
            | "CATEGORIES"
            | "STATUS"
            | "UID"
            | "URL"
            | "COMMENT"
            | "CONTACT"
            | "RESOURCES"
            | "X-WR-CALNAME"
            | "X-WR-CALDESC"
    )
}

fn table_names(connection: &Connection) -> Vec<String> {
    let Ok(mut statement) = connection.prepare(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT 50",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(0)) else {
        return Vec::new();
    };
    rows.filter_map(std::result::Result::ok).collect()
}

fn column_names(connection: &Connection, table: &str) -> Vec<String> {
    let pragma = format!("PRAGMA table_info({})", quote_ident(table));
    let Ok(mut statement) = connection.prepare(pragma.as_str()) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(1)) else {
        return Vec::new();
    };
    rows.filter_map(std::result::Result::ok).take(16).collect()
}

#[derive(Debug, Clone, Copy)]
struct SampleTarget<'a> {
    table: &'a str,
    columns: &'a [String],
}

fn append_samples(connection: &Connection, target: SampleTarget<'_>, parts: &mut Vec<String>) {
    if target.columns.is_empty() {
        return;
    }
    let selected = target
        .columns
        .iter()
        .map(|column| quote_ident(column.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT {selected} FROM {} LIMIT 32",
        quote_ident(target.table)
    );
    let Ok(mut statement) = connection.prepare(query.as_str()) else {
        return;
    };
    let column_count = target.columns.len();
    let Ok(mut rows) = statement.query([]) else {
        return;
    };
    while let Ok(Some(row)) = rows.next() {
        let mut values = Vec::new();
        for index in 0..column_count {
            if let Ok(value) = row.get::<_, String>(index) {
                let cleaned = collapse_whitespace(value.as_str());
                if !cleaned.is_empty() {
                    values.push(cleaned);
                }
            }
        }
        if !values.is_empty() {
            parts.push(values.join(" | "));
        }
    }
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
