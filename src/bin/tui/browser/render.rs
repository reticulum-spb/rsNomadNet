use super::*;
use rsnomadnet_core::browser::{Alignment, MicronStyle};
use tv::{Color, Style};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone)]
pub(super) struct Glyph {
    pub text: String,
    pub width: usize,
    pub style: Style,
    pub control: Option<String>,
    pub caret: bool,
}

#[derive(Clone)]
pub(super) enum Control {
    Link {
        target: String,
        fields: Vec<String>,
    },
    Input {
        name: String,
        value: String,
        width: usize,
        masked: bool,
        cursor: usize,
    },
    Choice {
        name: String,
        value: String,
        label: String,
        checked: bool,
        radio: bool,
    },
}

#[derive(Default)]
pub(super) struct Layout {
    pub rows: Vec<Vec<Glyph>>,
    pub controls: Vec<String>,
    pub anchors: HashMap<String, usize>,
    pub headings: Vec<usize>,
    pub partials: Vec<(String, MicronBlock)>,
    pub width: usize,
}

pub(super) fn colour(value: Option<&str>, fallback: Color) -> Color {
    value
        .and_then(|s| s.strip_prefix('#'))
        .filter(|s| s.len() == 6 && s.is_ascii())
        .and_then(|s| u32::from_str_radix(s, 16).ok())
        .map(|rgb| Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
        .unwrap_or(fallback)
}

pub(super) fn base_style(page: Option<&BrowserPage>) -> Style {
    Style::new(
        colour(page.and_then(|p| p.foreground.as_deref()), Color::Bios(7)),
        colour(page.and_then(|p| p.background.as_deref()), Color::Bios(0)),
    )
}

fn styled(base: Style, micron: &MicronStyle, heading: bool) -> Style {
    let mut style = Style::new(
        colour(micron.foreground.as_deref(), base.fg),
        colour(micron.background.as_deref(), base.bg),
    );
    style.modifiers.bold = heading || micron.bold;
    style.modifiers.italic = micron.italic;
    style.modifiers.underline = micron.underline;
    style
}

fn glyphs(text: &str, style: Style, control: Option<&str>) -> Vec<Glyph> {
    // Remote control characters are never interpreted as terminal operations.
    let safe: String = text
        .chars()
        .map(|ch| {
            if ch == '\t' {
                ' '
            } else if ch.is_control() {
                '�'
            } else {
                ch
            }
        })
        .collect();
    safe.graphemes(true)
        .map(|g| Glyph {
            text: g.into(),
            width: g.width().max(1),
            style,
            control: control.map(str::to_owned),
            caret: false,
        })
        .collect()
}

fn width(row: &[Glyph]) -> usize {
    row.iter().map(|g| g.width).sum()
}

fn wrap(tokens: Vec<Glyph>, available: usize) -> Vec<Vec<Glyph>> {
    let available = available.max(1);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    for mut token in tokens {
        if token.width > available {
            token.text = "�".into();
            token.width = 1;
        }
        if !row.is_empty() && width(&row) + token.width > available {
            // Prefer word boundaries, retaining styles and hit targets.
            if let Some(space) = row.iter().rposition(|g: &Glyph| g.text == " ") {
                let tail = row.split_off(space + 1);
                rows.push(std::mem::replace(&mut row, tail));
            } else {
                rows.push(std::mem::take(&mut row));
            }
            if width(&row) + token.width > available {
                rows.push(std::mem::take(&mut row));
            }
        }
        row.push(token);
    }
    rows.push(row);
    rows
}

fn align(
    mut row: Vec<Glyph>,
    available: usize,
    alignment: Alignment,
    indent: usize,
    style: Style,
) -> Vec<Glyph> {
    let extra = available.saturating_sub(width(&row));
    let padding = indent
        + match alignment {
            Alignment::Left => 0,
            Alignment::Center => extra / 2,
            Alignment::Right => extra,
        };
    let mut prefix = glyphs(&" ".repeat(padding), style, None);
    prefix.append(&mut row);
    prefix
}

fn slug(text: &str) -> String {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

struct Renderer<'a> {
    layout: Layout,
    form: &'a mut HashMap<String, Control>,
    fragments: &'a HashMap<String, Fragment>,
    columns: usize,
    style: Style,
    url: String,
}

impl Renderer<'_> {
    fn inline(&mut self, parts: &[Inline], path: &str, heading: bool) -> Vec<Glyph> {
        let mut output = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            let id = format!("{path}/i{index}");
            let mut caret = None;
            let (text, mut style, control) = match part {
                Inline::Anchor { name } => {
                    self.layout
                        .anchors
                        .entry(name.clone())
                        .or_insert(self.layout.rows.len());
                    continue;
                }
                Inline::Text { text, style } => {
                    (text.clone(), styled(self.style, style, heading), None)
                }
                Inline::Link {
                    label,
                    target,
                    fields,
                    style,
                } => {
                    self.form.insert(
                        id.clone(),
                        Control::Link {
                            target: resolve(&self.url, target).unwrap_or_else(|_| target.clone()),
                            fields: fields.clone(),
                        },
                    );
                    let mut rendered = styled(self.style, style, heading);
                    if style.foreground.is_none() {
                        rendered.fg = Color::Bios(11);
                    }
                    rendered.modifiers.underline = true;
                    (label.clone(), rendered, Some(id.clone()))
                }
                Inline::Input {
                    name,
                    value,
                    width,
                    masked,
                    style,
                } => {
                    let field = self
                        .form
                        .entry(id.clone())
                        .or_insert_with(|| Control::Input {
                            name: name.clone(),
                            value: value.clone(),
                            width: usize::from(*width).clamp(1, 256),
                            masked: *masked,
                            cursor: value.graphemes(true).count(),
                        });
                    let Control::Input {
                        value,
                        width,
                        masked,
                        cursor,
                        ..
                    } = field
                    else {
                        continue;
                    };
                    let chars: Vec<_> = value.graphemes(true).collect();
                    let end = (*cursor).min(chars.len());
                    let mut start = 0;
                    let mut before = if *masked {
                        end
                    } else {
                        chars[..end].iter().map(|g| g.width()).sum()
                    };
                    while start < end && before >= *width {
                        before =
                            before.saturating_sub(if *masked { 1 } else { chars[start].width() });
                        start += 1;
                    }
                    caret = Some(1 + before);
                    let mut visible = String::new();
                    for g in chars.iter().skip(start) {
                        let next = if *masked { "•" } else { g };
                        if visible.width() + next.width() >= *width {
                            break;
                        }
                        visible.push_str(next);
                    }
                    visible.push_str(&" ".repeat(width.saturating_sub(visible.width())));
                    let rendered = styled(self.style, style, heading);
                    (format!("[{visible}]"), rendered, Some(id.clone()))
                }
                Inline::Checkbox {
                    name,
                    value,
                    label,
                    checked,
                    style,
                }
                | Inline::Radio {
                    name,
                    value,
                    label,
                    checked,
                    style,
                } => {
                    let radio = matches!(part, Inline::Radio { .. });
                    let field = self
                        .form
                        .entry(id.clone())
                        .or_insert_with(|| Control::Choice {
                            name: name.clone(),
                            value: value.clone(),
                            label: label.clone(),
                            checked: *checked,
                            radio,
                        });
                    let Control::Choice { checked, label, .. } = field else {
                        continue;
                    };
                    let text = if radio {
                        format!("({}) {label}", if *checked { '*' } else { ' ' })
                    } else {
                        format!("[{}] {label}", if *checked { 'x' } else { ' ' })
                    };
                    (text, styled(self.style, style, heading), Some(id.clone()))
                }
            };
            if control.is_some() {
                self.layout.controls.push(id);
            }
            if heading {
                style.modifiers.bold = true;
            }
            let mut rendered = glyphs(&text, style, control.as_deref());
            if let Some(caret) = caret {
                let mut column = 0;
                for glyph in &mut rendered {
                    glyph.caret = column == caret;
                    column += glyph.width;
                }
            }
            output.extend(rendered);
        }
        output
    }

    fn blocks(&mut self, blocks: &[MicronBlock], prefix: &str, level: usize) {
        for (index, block) in blocks.iter().enumerate() {
            if self.layout.rows.len() >= 32768 {
                break;
            }
            let path = format!("{prefix}/{index}");
            match block {
                MicronBlock::Heading {
                    depth,
                    alignment,
                    parts,
                }
                | MicronBlock::Paragraph {
                    depth,
                    alignment,
                    parts,
                } => {
                    let heading = matches!(block, MicronBlock::Heading { .. });
                    let indent = (usize::from(depth.saturating_sub(1)) * 4)
                        .min(self.columns.saturating_sub(4));
                    let tokens = self.inline(parts, &path, heading);
                    if heading {
                        self.layout.headings.push(self.layout.rows.len());
                        let name =
                            slug(&tokens.iter().map(|g| g.text.as_str()).collect::<String>());
                        self.layout
                            .anchors
                            .entry(name)
                            .or_insert(self.layout.rows.len());
                    }
                    for row in wrap(tokens, self.columns.saturating_sub(indent)) {
                        self.layout.rows.push(align(
                            row,
                            self.columns.saturating_sub(indent),
                            *alignment,
                            indent,
                            self.style,
                        ));
                    }
                }
                MicronBlock::Divider { depth, character } => {
                    let indent = (usize::from(depth.saturating_sub(1)) * 4)
                        .min(self.columns.saturating_sub(1));
                    let count = (self.columns - indent) / character.to_string().width().max(1);
                    self.layout.rows.push(glyphs(
                        &format!(
                            "{}{}",
                            " ".repeat(indent),
                            character.to_string().repeat(count)
                        ),
                        self.style,
                        None,
                    ));
                }
                MicronBlock::Preformatted { text } => {
                    for line in text.split('\n') {
                        let mut expanded = String::new();
                        for ch in line.chars() {
                            if ch == '\t' {
                                expanded.push_str(&" ".repeat(4 - expanded.width() % 4));
                            } else {
                                expanded.push(ch);
                            }
                        }
                        self.layout.rows.push(glyphs(&expanded, self.style, None));
                    }
                }
                MicronBlock::Table {
                    rows,
                    alignment,
                    max_width,
                    column_alignments,
                } => {
                    let n = rows.iter().map(Vec::len).max().unwrap_or(0).min(64);
                    if n == 0 {
                        continue;
                    }
                    let table_width = max_width
                        .map(usize::from)
                        .unwrap_or(self.columns)
                        .min(self.columns)
                        .max(n * 2 - 1);
                    let cell_width = (table_width.saturating_sub((n - 1) * 3) / n).max(1);
                    for (r, cells) in rows.iter().enumerate() {
                        let rendered: Vec<_> = (0..n)
                            .map(|c| {
                                let tokens = self.inline(
                                    cells.get(c).map(Vec::as_slice).unwrap_or(&[]),
                                    &format!("{path}/r{r}/c{c}"),
                                    false,
                                );
                                wrap(tokens, cell_width)
                            })
                            .collect();
                        let height = rendered.iter().map(Vec::len).max().unwrap_or(1);
                        for y in 0..height {
                            let mut row = Vec::new();
                            for (c, lines) in rendered.iter().enumerate() {
                                if c > 0 {
                                    row.extend(glyphs(" │ ", self.style, None));
                                }
                                let cell = align(
                                    lines.get(y).cloned().unwrap_or_default(),
                                    cell_width,
                                    column_alignments.get(c).copied().unwrap_or_default(),
                                    0,
                                    self.style,
                                );
                                let padding = cell_width.saturating_sub(width(&cell));
                                row.extend(cell);
                                row.extend(glyphs(&" ".repeat(padding), self.style, None));
                            }
                            self.layout.rows.push(align(
                                row,
                                self.columns,
                                *alignment,
                                0,
                                self.style,
                            ));
                        }
                    }
                }
                MicronBlock::Partial { .. } => {
                    if level >= 4 || self.layout.partials.len() >= 64 {
                        self.layout
                            .rows
                            .push(glyphs("[Partial nesting limit]", self.style, None));
                        continue;
                    }
                    let mut descriptor = block.clone();
                    if let MicronBlock::Partial { target, .. } = &mut descriptor {
                        *target = resolve(&self.url, target).unwrap_or_else(|_| target.clone());
                    }
                    self.layout.partials.push((path.clone(), descriptor));
                    if let Some(fragment) = self.fragments.get(&path) {
                        if let Some(page) = &fragment.page {
                            let old_url = std::mem::replace(&mut self.url, page.url.clone());
                            self.blocks(&page.blocks, &format!("{path}/p"), level + 1);
                            self.url = old_url;
                        } else {
                            self.layout.rows.push(glyphs(
                                fragment.error.as_deref().unwrap_or("Loading partial…"),
                                self.style,
                                None,
                            ));
                        }
                    } else {
                        self.layout
                            .rows
                            .push(glyphs("Loading partial…", self.style, None));
                    }
                }
            }
        }
    }
}

pub(super) fn render(
    page: &BrowserPage,
    columns: usize,
    form: &mut HashMap<String, Control>,
    fragments: &HashMap<String, Fragment>,
) -> Layout {
    let mut renderer = Renderer {
        layout: Layout::default(),
        form,
        fragments,
        columns: columns.max(1),
        style: base_style(Some(page)),
        url: page.url.clone(),
    };
    renderer.blocks(&page.blocks, "page", 0);
    renderer.layout.width = renderer
        .layout
        .rows
        .iter()
        .map(|row| width(row))
        .max()
        .unwrap_or(0);
    renderer.layout
}
