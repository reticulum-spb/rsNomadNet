use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NomadUrl {
    pub destination_hash: [u8; 16],
    pub path: String,
}

impl NomadUrl {
    pub fn parse(value: &str) -> Result<Self, BrowserError> {
        let value = value
            .trim()
            .strip_prefix("nomadnetwork://")
            .unwrap_or(value.trim());
        let (hash, path) = match value.split_once(':') {
            Some(parts) => parts,
            None => (value, ""),
        };
        let bytes = hex::decode(hash).map_err(|_| BrowserError::InvalidUrl)?;
        if bytes.len() != 16 {
            return Err(BrowserError::InvalidUrl);
        }
        let mut destination_hash = [0u8; 16];
        destination_hash.copy_from_slice(&bytes);
        let path = if path.is_empty() {
            "/page/index.mu".to_string()
        } else if path.starts_with("/page/") || path.starts_with("/file/") {
            path.to_string()
        } else {
            return Err(BrowserError::UnsupportedPath);
        };
        if path.contains('\0') || path.len() > 2048 {
            return Err(BrowserError::InvalidUrl);
        }
        Ok(Self {
            destination_hash,
            path,
        })
    }

    pub fn canonical(&self) -> String {
        format!("{}:{}", hex::encode(self.destination_hash), self.path)
    }

    pub fn is_page(&self) -> bool {
        self.path.starts_with("/page/")
    }

    pub fn is_file(&self) -> bool {
        self.path.starts_with("/file/")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("invalid NomadNet URL")]
    InvalidUrl,
    #[error("only /page/ and /file/ paths are supported")]
    UnsupportedPath,
    #[error("page is not valid UTF-8")]
    InvalidEncoding,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserPage {
    pub url: String,
    pub title: Option<String>,
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cache_seconds: u64,
    pub from_cache: bool,
    pub blocks: Vec<MicronBlock>,
}

pub struct DownloadedFile {
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MicronBlock {
    Heading { depth: u8, parts: Vec<Inline> },
    Paragraph { parts: Vec<Inline> },
    Divider,
    Preformatted { text: String },
    Table { rows: Vec<Vec<Vec<Inline>>> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inline {
    Text {
        text: String,
    },
    Link {
        label: String,
        target: String,
        fields: Vec<String>,
    },
    Input {
        name: String,
        value: String,
        width: u16,
        masked: bool,
    },
    Checkbox {
        name: String,
        value: String,
        label: String,
        checked: bool,
    },
    Radio {
        name: String,
        value: String,
        label: String,
        checked: bool,
    },
}

pub fn parse_page(
    url: String,
    bytes: &[u8],
    from_cache: bool,
) -> Result<BrowserPage, BrowserError> {
    let source = std::str::from_utf8(bytes).map_err(|_| BrowserError::InvalidEncoding)?;
    let mut cache_seconds = 12 * 60 * 60;
    let mut foreground = None;
    let mut background = None;
    let mut blocks = Vec::new();
    let mut literal = false;
    let mut table_rows: Option<Vec<Vec<Vec<Inline>>>> = None;

    for line in source.lines() {
        if let Some(value) = line.strip_prefix("#!c=") {
            cache_seconds = value.trim().parse().unwrap_or(cache_seconds);
            continue;
        }
        if let Some(value) = line.strip_prefix("#!fg=") {
            foreground = valid_colour(value.trim());
            continue;
        }
        if let Some(value) = line.strip_prefix("#!bg=") {
            background = valid_colour(value.trim());
            continue;
        }
        if line == "`=" {
            literal = !literal;
            continue;
        }
        if line.starts_with("`t") {
            if let Some(rows) = table_rows.take() {
                if !rows.is_empty() {
                    blocks.push(MicronBlock::Table { rows });
                }
            } else {
                table_rows = Some(Vec::new());
            }
            continue;
        }
        if let Some(rows) = table_rows.as_mut() {
            rows.push(
                line.trim()
                    .trim_matches('|')
                    .split('|')
                    .map(|cell| parse_inline(cell.trim()))
                    .collect(),
            );
            continue;
        }
        if literal {
            blocks.push(MicronBlock::Preformatted {
                text: line.to_string(),
            });
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if line == "-" || (line.len() == 2 && line.starts_with('-')) {
            blocks.push(MicronBlock::Divider);
            continue;
        }
        let depth = line
            .chars()
            .take_while(|character| *character == '>')
            .count();
        let content = if depth > 0 { &line[depth..] } else { line };
        if content.is_empty() {
            continue;
        }
        let parts = parse_inline(content);
        if depth > 0 {
            blocks.push(MicronBlock::Heading {
                depth: depth.min(6) as u8,
                parts,
            });
        } else {
            blocks.push(MicronBlock::Paragraph { parts });
        }
    }
    if let Some(rows) = table_rows
        && !rows.is_empty()
    {
        blocks.push(MicronBlock::Table { rows });
    }
    let title = blocks.iter().find_map(|block| match block {
        MicronBlock::Heading { parts, .. } => Some(inline_text(parts)),
        _ => None,
    });
    Ok(BrowserPage {
        url,
        title,
        foreground,
        background,
        cache_seconds,
        from_cache,
        blocks,
    })
}

fn parse_inline(line: &str) -> Vec<Inline> {
    let characters: Vec<char> = line.chars().collect();
    let mut output = Vec::new();
    let mut text = String::new();
    let mut index = 0;
    let mut formatting = false;
    while index < characters.len() {
        if characters[index] == '`' {
            formatting = true;
            index += 1;
            continue;
        }
        if formatting && characters[index] == '[' {
            if let Some(relative_end) = characters[index + 1..]
                .iter()
                .position(|character| *character == ']')
            {
                if !text.is_empty() {
                    output.push(Inline::Text {
                        text: std::mem::take(&mut text),
                    });
                }
                let value: String = characters[index + 1..index + 1 + relative_end]
                    .iter()
                    .collect();
                let mut components = value.split('`');
                let first = components.next().unwrap_or_default();
                let second = components.next();
                let fields = components
                    .next()
                    .map(|value| {
                        value
                            .split('|')
                            .filter(|field| !field.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let (label, target) = match second {
                    Some(target) => (first, target),
                    None => (first, first),
                };
                if !target.is_empty() {
                    output.push(Inline::Link {
                        label: if label.is_empty() { target } else { label }.to_string(),
                        target: target.to_string(),
                        fields,
                    });
                }
                index += relative_end + 2;
                formatting = false;
                continue;
            }
        }
        if formatting
            && characters[index] == '<'
            && let Some((field, consumed)) = parse_field(&characters[index..])
        {
            if !text.is_empty() {
                output.push(Inline::Text {
                    text: std::mem::take(&mut text),
                });
            }
            output.push(field);
            index += consumed;
            formatting = false;
            continue;
        }
        if formatting {
            let command = characters[index];
            let skip = match command {
                'F' | 'B' if characters.get(index + 1) == Some(&'T') => 8,
                'F' | 'B' => 4,
                ':' => {
                    let mut length = 1;
                    while characters.get(index + length).is_some_and(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                    }) {
                        length += 1;
                    }
                    length
                }
                _ => 1,
            };
            index = (index + skip).min(characters.len());
            formatting = false;
            continue;
        }
        text.push(characters[index]);
        index += 1;
    }
    if !text.is_empty() {
        output.push(Inline::Text { text });
    }
    output
}

fn parse_field(characters: &[char]) -> Option<(Inline, usize)> {
    let tick = characters.iter().position(|character| *character == '`')?;
    let end = characters
        .iter()
        .enumerate()
        .skip(tick + 1)
        .find(|(_, character)| **character == '>')?
        .0;
    let descriptor: String = characters[1..tick].iter().collect();
    let label: String = characters[tick + 1..end].iter().collect();
    let components: Vec<&str> = descriptor.split('|').collect();
    let flags = if components.len() > 1 {
        components[0]
    } else {
        ""
    };
    let name = if components.len() > 1 {
        components[1]
    } else {
        components[0]
    };
    if name.is_empty() || !valid_field_name(name) {
        return None;
    }
    let value = components.get(2).copied().unwrap_or(&label).to_string();
    let checked = components.get(3).is_some_and(|value| *value == "*");
    let field = if flags.contains('?') {
        Inline::Checkbox {
            name: name.to_string(),
            value,
            label,
            checked,
        }
    } else if flags.contains('^') {
        Inline::Radio {
            name: name.to_string(),
            value,
            label,
            checked,
        }
    } else {
        let width = flags
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u16>()
            .unwrap_or(24)
            .clamp(1, 256);
        Inline::Input {
            name: name.to_string(),
            value: label,
            width,
            masked: flags.contains('!'),
        }
    };
    Some((field, end + 1))
}

fn valid_field_name(value: &str) -> bool {
    value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn valid_colour(value: &str) -> Option<String> {
    if (value.len() == 3 || value.len() == 6)
        && value.chars().all(|character| character.is_ascii_hexdigit())
    {
        let value = value.to_ascii_lowercase();
        if value.len() == 3 {
            Some(format!(
                "#{}{}{}{}{}{}",
                &value[0..1],
                &value[0..1],
                &value[1..2],
                &value[1..2],
                &value[2..3],
                &value[2..3]
            ))
        } else {
            Some(format!("#{value}"))
        }
    } else {
        None
    }
}

fn inline_text(parts: &[Inline]) -> String {
    parts
        .iter()
        .map(|part| match part {
            Inline::Text { text } => text.as_str(),
            Inline::Link { label, .. } => label.as_str(),
            Inline::Input { value, .. } => value.as_str(),
            Inline::Checkbox { label, .. } | Inline::Radio { label, .. } => label.as_str(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nomad_urls_and_default_path() {
        let url = NomadUrl::parse("11111111111111111111111111111111").unwrap();
        assert_eq!(url.path, "/page/index.mu");
        assert_eq!(
            url.canonical(),
            "11111111111111111111111111111111:/page/index.mu"
        );
        let file = NomadUrl::parse("11111111111111111111111111111111:/file/a").unwrap();
        assert!(file.is_file());
    }

    #[test]
    fn parses_safe_micron_subset() {
        let page = parse_page(
            "node:/page/index.mu".into(),
            b"#!c=60\n#!fg=abc\n>Heading\nText `[Next`:/page/next.mu]\n-\n",
            false,
        )
        .unwrap();
        assert_eq!(page.cache_seconds, 60);
        assert_eq!(page.foreground.as_deref(), Some("#aabbcc"));
        assert_eq!(page.title.as_deref(), Some("Heading"));
        assert_eq!(page.blocks.len(), 3);
    }

    #[test]
    fn parses_micron_table() {
        let page = parse_page(
            "node:/page/table.mu".into(),
            b"`t\nName | Value\none | 1\n`t\n",
            false,
        )
        .unwrap();
        let MicronBlock::Table { rows } = &page.blocks[0] else {
            panic!("expected table");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
    }

    #[test]
    fn parses_form_fields_and_submit_metadata() {
        let page = parse_page(
            "node:/page/form.mu".into(),
            b"Name: `<24|name`Alice>\n`<?|news|yes|*`Subscribe>\n`[Send`:/page/result.mu`name|news]\n",
            false,
        )
        .unwrap();
        let MicronBlock::Paragraph { parts } = &page.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(matches!(
            &parts[1],
            Inline::Input { name, value, .. } if name == "name" && value == "Alice"
        ));
        let MicronBlock::Paragraph { parts } = &page.blocks[2] else {
            panic!("expected submit paragraph");
        };
        assert!(matches!(
            &parts[0],
            Inline::Link { fields, .. } if fields == &["name", "news"]
        ));
    }

    #[test]
    fn field_after_colour_command_matches_nomadnet_page() {
        let page = parse_page(
            "node:/page/index.mu".into(),
            b"> RNS-Gate Rust node pages\n\nUser name: `B444`<username`Anonymous>`b\n\n`[Submit`:/page/hello.mu`*]\n",
            false,
        )
        .unwrap();
        let MicronBlock::Paragraph { parts } = &page.blocks[1] else {
            panic!("expected field paragraph");
        };
        assert!(matches!(
            &parts[1],
            Inline::Input { name, value, .. } if name == "username" && value == "Anonymous"
        ));
        let MicronBlock::Paragraph { parts } = &page.blocks[2] else {
            panic!("expected submit paragraph");
        };
        assert!(matches!(
            &parts[0],
            Inline::Link { fields, .. } if fields == &["*"]
        ));
    }
}
