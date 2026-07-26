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
    Heading {
        depth: u8,
        alignment: Alignment,
        parts: Vec<Inline>,
    },
    Paragraph {
        depth: u8,
        alignment: Alignment,
        parts: Vec<Inline>,
    },
    Divider {
        depth: u8,
        character: char,
    },
    Preformatted {
        text: String,
    },
    Table {
        alignment: Alignment,
        max_width: Option<u16>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Partial {
        target: String,
        interval_seconds: u64,
        fields: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicronStyle {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub bold: bool,
    pub underline: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inline {
    Text {
        text: String,
        style: MicronStyle,
    },
    Link {
        label: String,
        target: String,
        fields: Vec<String>,
        style: MicronStyle,
    },
    Input {
        name: String,
        value: String,
        width: u16,
        masked: bool,
        style: MicronStyle,
    },
    Checkbox {
        name: String,
        value: String,
        label: String,
        checked: bool,
        style: MicronStyle,
    },
    Radio {
        name: String,
        value: String,
        label: String,
        checked: bool,
        style: MicronStyle,
    },
    Anchor {
        name: String,
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
    let mut table_alignment = Alignment::Left;
    let mut table_max_width = None;
    let mut alignment = Alignment::Left;
    let mut depth = 0u8;
    let mut style = MicronStyle::default();

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
        if !literal && let Some(partial) = parse_partial(line) {
            blocks.push(partial);
            continue;
        }
        if let Some(options) = line.strip_prefix("`t") {
            if let Some(rows) = table_rows.take() {
                if !rows.is_empty() {
                    blocks.push(MicronBlock::Table {
                        alignment: table_alignment,
                        max_width: table_max_width,
                        rows,
                    });
                }
            } else {
                let mut options = options.trim();
                table_alignment = match options.chars().next() {
                    Some('c') => {
                        options = &options[1..];
                        Alignment::Center
                    }
                    Some('r') => {
                        options = &options[1..];
                        Alignment::Right
                    }
                    Some('l') => {
                        options = &options[1..];
                        Alignment::Left
                    }
                    _ => alignment,
                };
                table_max_width = options.parse::<u16>().ok().map(|width| width.clamp(1, 512));
                table_rows = Some(Vec::new());
            }
            continue;
        }
        if let Some(rows) = table_rows.as_mut() {
            rows.push(
                line.trim()
                    .trim_matches('|')
                    .split('|')
                    .map(|cell| parse_inline(cell.trim(), &mut style))
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
        if line.starts_with('-') {
            blocks.push(MicronBlock::Divider {
                depth,
                character: line.chars().nth(1).unwrap_or('─'),
            });
            continue;
        }
        let heading_depth = line
            .chars()
            .take_while(|character| *character == '>')
            .count();
        if heading_depth > 0 {
            depth = heading_depth.min(255) as u8;
        }
        let mut content = if heading_depth > 0 {
            &line[heading_depth..]
        } else {
            line
        };
        if heading_depth == 0
            && let Some(reset) = content.strip_prefix('<')
        {
            depth = 0;
            content = reset;
        }
        if heading_depth == 0 {
            match content.get(..2) {
                Some("`c") => {
                    alignment = Alignment::Center;
                    content = &content[2..];
                }
                Some("`r") => {
                    alignment = Alignment::Right;
                    content = &content[2..];
                }
                Some("`l") => {
                    alignment = Alignment::Left;
                    content = &content[2..];
                }
                Some("`a") => {
                    alignment = Alignment::Left;
                    content = &content[2..];
                }
                _ => {}
            }
        }
        if content.is_empty() {
            continue;
        }
        let parts = parse_inline(content, &mut style);
        if heading_depth > 0 {
            blocks.push(MicronBlock::Heading {
                depth,
                alignment,
                parts,
            });
        } else {
            blocks.push(MicronBlock::Paragraph {
                depth,
                alignment,
                parts,
            });
        }
    }
    if let Some(rows) = table_rows
        && !rows.is_empty()
    {
        blocks.push(MicronBlock::Table {
            alignment: table_alignment,
            max_width: table_max_width,
            rows,
        });
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

fn parse_inline(line: &str, style: &mut MicronStyle) -> Vec<Inline> {
    let characters: Vec<char> = line.chars().collect();
    let mut output = Vec::new();
    let mut text = String::new();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '\\' && index + 1 < characters.len() {
            text.push(characters[index + 1]);
            index += 2;
            continue;
        }
        if characters[index] != '`' || index + 1 >= characters.len() {
            text.push(characters[index]);
            index += 1;
            continue;
        }
        let command_index = index + 1;
        let command = characters[command_index];
        if command == '[' {
            if let Some(relative_end) = characters[command_index + 1..]
                .iter()
                .position(|character| *character == ']')
            {
                push_text(&mut output, &mut text, style);
                let value: String = characters[command_index + 1..command_index + 1 + relative_end]
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
                        style: style.clone(),
                    });
                }
                index = command_index + relative_end + 2;
                continue;
            }
        }
        if command == '<'
            && let Some((field, consumed)) = parse_field(&characters[command_index..], style)
        {
            push_text(&mut output, &mut text, style);
            output.push(field);
            index += consumed + 1;
            continue;
        }
        push_text(&mut output, &mut text, style);
        match command {
            '!' => style.bold = !style.bold,
            '_' => style.underline = !style.underline,
            '*' => style.italic = !style.italic,
            'f' => style.foreground = None,
            'b' => style.background = None,
            '`' => *style = MicronStyle::default(),
            'F' | 'B' => {
                let true_colour = characters.get(command_index + 1) == Some(&'T');
                let colour_start = command_index + if true_colour { 2 } else { 1 };
                let colour_length = if true_colour { 6 } else { 3 };
                if colour_start + colour_length <= characters.len() {
                    let colour: String = characters[colour_start..colour_start + colour_length]
                        .iter()
                        .collect();
                    if let Some(colour) = valid_colour(&colour) {
                        if command == 'F' {
                            style.foreground = Some(colour);
                        } else {
                            style.background = Some(colour);
                        }
                        index = colour_start + colour_length;
                        continue;
                    }
                }
            }
            ':' => {
                let mut end = command_index + 1;
                while characters.get(end).is_some_and(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                }) {
                    end += 1;
                }
                if end > command_index + 1 {
                    output.push(Inline::Anchor {
                        name: characters[command_index + 1..end].iter().collect(),
                    });
                }
                index = end;
                continue;
            }
            _ => {}
        }
        index += 2;
    }
    push_text(&mut output, &mut text, style);
    output
}

fn parse_partial(line: &str) -> Option<MicronBlock> {
    let value = line.strip_prefix("`{")?.strip_suffix('}')?;
    let mut components = value.split('`');
    let target = components.next()?.trim();
    if target.is_empty() {
        return None;
    }
    let interval_seconds = components
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(24 * 60 * 60);
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
    Some(MicronBlock::Partial {
        target: target.to_string(),
        interval_seconds,
        fields,
    })
}

fn push_text(output: &mut Vec<Inline>, text: &mut String, style: &MicronStyle) {
    if !text.is_empty() {
        output.push(Inline::Text {
            text: std::mem::take(text),
            style: style.clone(),
        });
    }
}

fn parse_field(characters: &[char], style: &MicronStyle) -> Option<(Inline, usize)> {
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
            style: style.clone(),
        }
    } else if flags.contains('^') {
        Inline::Radio {
            name: name.to_string(),
            value,
            label,
            checked,
            style: style.clone(),
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
            style: style.clone(),
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
            Inline::Text { text, .. } => text.as_str(),
            Inline::Link { label, .. } => label.as_str(),
            Inline::Input { value, .. } => value.as_str(),
            Inline::Checkbox { label, .. } | Inline::Radio { label, .. } => label.as_str(),
            Inline::Anchor { .. } => "",
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
        let MicronBlock::Table { rows, .. } = &page.blocks[0] else {
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
        let MicronBlock::Paragraph { parts, .. } = &page.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(matches!(
            &parts[1],
            Inline::Input { name, value, .. } if name == "name" && value == "Alice"
        ));
        let MicronBlock::Paragraph { parts, .. } = &page.blocks[2] else {
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
        let MicronBlock::Paragraph { parts, .. } = &page.blocks[1] else {
            panic!("expected field paragraph");
        };
        assert!(matches!(
            &parts[1],
            Inline::Input { name, value, .. } if name == "username" && value == "Anonymous"
        ));
        let MicronBlock::Paragraph { parts, .. } = &page.blocks[2] else {
            panic!("expected submit paragraph");
        };
        assert!(matches!(
            &parts[0],
            Inline::Link { fields, .. } if fields == &["*"]
        ));
    }

    #[test]
    fn preserves_inline_styles_alignment_sections_and_anchors() {
        let page = parse_page(
            "node:/page/style.mu".into(),
            b"`c`Ff00`!Bold`! plain\n>>Nested\n`:note anchored\n<Back\n",
            false,
        )
        .unwrap();
        let MicronBlock::Paragraph {
            alignment, parts, ..
        } = &page.blocks[0]
        else {
            panic!("expected styled paragraph");
        };
        assert_eq!(*alignment, Alignment::Center);
        assert!(matches!(
            &parts[0],
            Inline::Text { text, style }
                if text == "Bold" && style.bold && style.foreground.as_deref() == Some("#ff0000")
        ));
        let MicronBlock::Heading { depth, .. } = &page.blocks[1] else {
            panic!("expected nested heading");
        };
        assert_eq!(*depth, 2);
        let MicronBlock::Paragraph { depth, parts, .. } = &page.blocks[2] else {
            panic!("expected nested anchor");
        };
        assert_eq!(*depth, 2);
        assert!(matches!(&parts[0], Inline::Anchor { name } if name == "note"));
        let MicronBlock::Paragraph { depth, .. } = &page.blocks[3] else {
            panic!("expected reset paragraph");
        };
        assert_eq!(*depth, 0);
    }

    #[test]
    fn parses_table_alignment_and_width() {
        let page = parse_page("node:/page/table.mu".into(), b"`tc30\nA | B\n`t\n", false).unwrap();
        assert!(matches!(
            &page.blocks[0],
            MicronBlock::Table {
                alignment: Alignment::Center,
                max_width: Some(30),
                ..
            }
        ));
    }

    #[test]
    fn parses_refreshing_partial_with_fields() {
        let page = parse_page(
            "node:/page/partial.mu".into(),
            b"`{11111111111111111111111111111111:/page/status.mu`10`pid=32|user_name}\n",
            false,
        )
        .unwrap();
        assert!(matches!(
            &page.blocks[0],
            MicronBlock::Partial {
                target,
                interval_seconds: 10,
                fields,
            } if target.ends_with("/page/status.mu")
                && fields == &["pid=32", "user_name"]
        ));
    }
}
