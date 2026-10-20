//! Apple Music TTML → модель строк/слов приложения.
//!
//! Это формат, который использует Spicy Lyrics: `<p begin end>` — строка, `<span begin end>` —
//! слово или слог внутри строки. Строки с `ttm:role="x-bg"` (бэк-вокал) и слоги внутри них
//! пока не отдаются: модель строки их не знает, а показывать их как обычный текст нельзя.
//! Времена — `HH:MM:SS.mmm`, `MM:SS.mmm` или секунды; кадры (`HH:MM:SS:FF`) пропускаются.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, BytesText, Event};

use crate::{Line, Word, round3};

struct Span {
    begin: Option<f64>,
    end: Option<f64>,
    /// Слог/слово бэк-вокала (сам или внутри `x-bg`).
    bg: bool,
    text: String,
    has_child: bool,
}

struct Segment {
    text: String,
    begin: Option<f64>,
    end: Option<f64>,
}

/// `None` — это не TTML с поющимся текстом. `bool` — было ли у слов собственное время
/// (а не только у строки): от этого зависит `kind` результата.
pub fn parse_ttml(xml: &str) -> Option<(Vec<Line>, bool)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    xml_text(&mut reader)
}

fn xml_text(reader: &mut Reader<&[u8]>) -> Option<(Vec<Line>, bool)> {
    let mut lines = Vec::new();
    let mut background: Vec<(f64, f64, Vec<Word>)> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut segments: Vec<Segment> = Vec::new();
    let mut bg_segments: Vec<Segment> = Vec::new();
    let mut in_line = false;
    let mut line_begin: Option<f64> = None;
    let mut line_end: Option<f64> = None;
    let mut line_bg = false;
    let mut agent: Option<String> = None;
    let mut last_bg = false;
    let mut any_word_timing = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"p" => {
                    if in_line {
                        return None; // вложенные строки не поддерживаются
                    }
                    in_line = true;
                    line_begin = attr(&e, b"begin").as_deref().and_then(parse_time);
                    line_end = attr(&e, b"end").as_deref().and_then(parse_time);
                    line_bg = attr(&e, b"role").as_deref() == Some("x-bg");
                    agent = attr(&e, b"agent").filter(|value| !value.is_empty());
                    segments.clear();
                    bg_segments.clear();
                    spans.clear();
                    last_bg = false;
                }
                b"span" if in_line => {
                    let parent_bg = spans.last().is_some_and(|span| span.bg);
                    if let Some(parent) = spans.last_mut() {
                        parent.has_child = true;
                    }
                    spans.push(Span {
                        begin: attr(&e, b"begin").as_deref().and_then(parse_time),
                        end: attr(&e, b"end").as_deref().and_then(parse_time),
                        bg: parent_bg || attr(&e, b"role").as_deref() == Some("x-bg"),
                        text: String::new(),
                        has_child: false,
                    });
                }
                _ => {}
            },
            Ok(Event::Text(text)) => {
                if let Some(value) = decoded(&text) {
                    append(&mut spans, &mut segments, &mut bg_segments, last_bg, &value);
                }
            }
            Ok(Event::GeneralRef(reference)) => {
                if let Some(value) = reference_char(&reference) {
                    append(&mut spans, &mut segments, &mut bg_segments, last_bg, &value.to_string());
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"span" if in_line => {
                    let span = spans.pop()?;
                    last_bg = span.bg;
                    if !span.has_child && !span.text.is_empty() {
                        let segment = Segment { text: span.text, begin: span.begin, end: span.end };
                        if span.bg {
                            bg_segments.push(segment);
                        } else {
                            segments.push(segment);
                        }
                    }
                }
                b"p" if in_line => {
                    if line_bg {
                        // every span of an x-bg <p> is background, regardless of its own role
                        if let Some(words) = build_words(&segments, line_begin, line_end) {
                            let from = line_begin.unwrap_or_else(|| words.first().map_or(0.0, |word| word.s));
                            let to = line_end.unwrap_or_else(|| words.last().map_or(from, |word| word.e));
                            background.push((from, to, words));
                        }
                    } else if let Some(words) = build_words(&segments, line_begin, line_end) {
                        let bg = build_words(&bg_segments, line_begin, line_end);
                        let t = round3(line_begin.unwrap_or(words[0].s));
                        let e = round3(line_end.unwrap_or(words[words.len() - 1].e).max(t));
                        let line = Line::Words { t, e, w: words, i: false, bg, agent: agent.take() };
                        any_word_timing |= has_word_timing(&line);
                        lines.push(line);
                    }
                    in_line = false;
                    segments.clear();
                    bg_segments.clear();
                    spans.clear();
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return None,
        }
    }

    attach_background(&mut lines, background);
    (!lines.is_empty()).then_some((lines, any_word_timing))
}

/// A background line (`x-bg`) belongs to the lead line it overlaps in time; without an
/// overlap there is nothing to put it under, so it is dropped.
fn attach_background(lines: &mut [Line], background: Vec<(f64, f64, Vec<Word>)>) {
    for (from, to, words) in background {
        let mut best: Option<(usize, f64)> = None;
        for (index, line) in lines.iter().enumerate() {
            if let Line::Words { t, e, .. } = line {
                let overlap = to.min(*e) - from.max(*t);
                if overlap > 0.0 && best.map_or(true, |(_, best)| overlap > best) {
                    best = Some((index, overlap));
                }
            }
        }
        let Some((index, _)) = best else { continue };
        if let Line::Words { bg, .. } = &mut lines[index] {
            let mut merged = bg.take().unwrap_or_default();
            merged.extend(words);
            merged.sort_by(|a, b| a.s.partial_cmp(&b.s).unwrap_or(std::cmp::Ordering::Equal));
            *bg = Some(merged);
        }
    }
}

fn append(spans: &mut [Span], segments: &mut [Segment], background: &mut [Segment], last_bg: bool, text: &str) {
    if let Some(span) = spans.last_mut() {
        span.text.push_str(text);
    } else {
        // между <span> в TTML лежит или разделитель-пробел, или форматирование XML
        // (переводы строк и отступы): форматирование в текст песни не попадает.
        let target = if last_bg { background } else { segments };
        if let Some(segment) = target.last_mut() {
            if text.chars().all(char::is_whitespace) {
                if text == " " {
                    segment.text.push(' ');
                }
            } else {
                segment.text.push_str(text);
            }
        }
    }
}

fn decoded(text: &BytesText) -> Option<String> {
    text.decode().ok().map(|value| value.into_owned())
}

fn reference_char(reference: &quick_xml::events::BytesRef) -> Option<char> {
    if let Ok(Some(ch)) = reference.resolve_char_ref() {
        return Some(ch);
    }
    match reference.decode().ok()?.as_ref() {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

fn attr(element: &BytesStart, name: &[u8]) -> Option<String> {
    for attribute in element.attributes().flatten() {
        if attribute.key.local_name().as_ref() == name {
            return Some(String::from_utf8_lossy(&attribute.value).into_owned());
        }
    }
    None
}

/// `HH:MM:SS.mmm`, `MM:SS.mmm`, `SS.mmm`, with an optional `s`/`ms` suffix.
fn parse_time(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (raw, scale) = if let Some(value) = raw.strip_suffix("ms") {
        (value, 0.001)
    } else if let Some(value) = raw.strip_suffix('s') {
        (value, 1.0)
    } else {
        (raw, 1.0)
    };
    let (clock, fraction) = match raw.rsplit_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (raw, None),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None; // frames (HH:MM:SS:FF) are not supported
    }
    let mut seconds = 0.0;
    for (index, part) in parts.iter().rev().enumerate() {
        let value: f64 = part.parse().ok()?;
        seconds += value * 60f64.powi(index as i32);
    }
    if let Some(fraction) = fraction {
        seconds += format!("0.{fraction}").parse::<f64>().ok()?;
    }
    seconds.is_finite().then_some(seconds * scale)
}

fn build_words(segments: &[Segment], begin: Option<f64>, end: Option<f64>) -> Option<Vec<Word>> {
    if segments.is_empty() {
        return None;
    }
    let fallback_start = begin.unwrap_or(0.0);
    let fallback_end = end.unwrap_or(fallback_start);
    // символ + его время + номер сегмента (для слогов)
    let mut chars: Vec<(char, f64, f64, usize)> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let len = segment.text.chars().count().max(1);
        let start = segment.begin.unwrap_or(fallback_start);
        let finish = segment.end.unwrap_or(fallback_end).max(start);
        for (offset, ch) in segment.text.chars().enumerate() {
            let from = start + (finish - start) * offset as f64 / len as f64;
            let to = start + (finish - start) * (offset + 1) as f64 / len as f64;
            chars.push((ch, from, to, index));
        }
    }
    if chars.is_empty() {
        return None;
    }

    let mut words = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at].0.is_whitespace() {
            at += 1;
            continue;
        }
        let start = at;
        while at < chars.len() && !chars[at].0.is_whitespace() {
            at += 1;
        }
        let mut finish = at;
        if at < chars.len() && chars[at].0.is_whitespace() {
            finish = at + 1; // пробел остаётся в x, как у остальных источников
        }
        let text: String = chars[start..finish].iter().map(|c| c.0).collect();
        if text.trim().is_empty() || text.trim() == "\u{266a}" {
            continue;
        }
        let mut parts: Vec<(u32, f64, f64)> = Vec::new();
        let mut last_segment: Option<usize> = None;
        for &(ch, from, to, segment) in &chars[start..finish] {
            if ch.is_whitespace() {
                continue;
            }
            if last_segment == Some(segment) {
                if let Some(part) = parts.last_mut() {
                    part.0 += 1;
                    part.2 = to;
                    continue;
                }
            }
            parts.push((1, from, to));
            last_segment = Some(segment);
        }
        words.push(Word {
            x: text,
            s: round3(chars[start].1),
            e: round3(chars[finish - 1].2),
            p: (parts.len() > 1).then_some(parts),
        });
    }
    (!words.is_empty()).then_some(words)
}

fn has_word_timing(line: &Line) -> bool {
    match line {
        Line::Gap { .. } => false,
        // one word with syllable parts still animates inside the word; one plain word does not
        Line::Words { w, .. } => {
            !w.is_empty()
                && w.iter().all(|word| word.e > word.s)
                && (w.len() > 1 || w.iter().any(|word| word.p.is_some()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
<body><div>
  <p begin="00:00:10.000" end="00:00:12.500">
    <span begin="00:00:10.000" end="00:00:10.400">Con</span><span begin="00:00:10.400" end="00:00:10.800">gra</span><span begin="00:00:10.800" end="00:00:11.200">tu</span><span begin="00:00:11.200" end="00:00:11.700">la</span><span begin="00:00:11.700" end="00:00:12.500">tions</span> <span begin="00:00:12.500" end="00:00:12.500"> </span>
  </p>
  <p begin="00:00:13.000" end="00:00:14.000" ttm:role="x-bg">
    <span begin="00:00:13.000" end="00:00:14.000">background</span>
  </p>
  <p begin="00:00:15.000" end="00:00:16.000">
    <span begin="00:00:15.000" end="00:00:15.500">one</span> <span begin="00:00:15.500" end="00:00:16.000">two</span>
  </p>
</div></body></tt>"#;

    #[test]
    fn syllable_spans_become_one_word_with_parts() {
        let (lines, word_timing) = parse_ttml(SAMPLE).expect("ttml");
        assert!(word_timing);
        assert_eq!(lines.len(), 2, "the x-bg line is skipped");
        let Line::Words { t, e, w, i, .. } = &lines[0] else { panic!("words") };
        assert_eq!((*t, *e), (10.0, 12.5));
        assert!(!i);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].x, "Congratulations ");
        assert!((w[0].s - 10.0).abs() < 0.001);
        assert!((w[0].e - 12.5).abs() < 0.001);
        let parts = w[0].p.as_ref().expect("five syllables");
        assert_eq!(parts.len(), 5);
        assert_eq!(parts.iter().map(|part| part.0).sum::<u32>(), 15);
    }

    #[test]
    fn words_keep_their_own_times() {
        let (lines, _) = parse_ttml(SAMPLE).expect("ttml");
        let Line::Words { w, .. } = &lines[1] else { panic!("words") };
        assert_eq!(w.len(), 2);
        assert_eq!((w[0].x.as_str(), w[0].s, w[0].e), ("one ", 15.0, 15.5));
        assert_eq!((w[1].x.as_str(), w[1].s, w[1].e), ("two", 15.5, 16.0));
        assert!(w.iter().all(|word| word.p.is_none()), "one span per word");
    }

    #[test]
    fn background_and_duet_are_attached() {
        let xml = r#"<tt><body><div>
          <p begin="1.0" end="4.0" ttm:agent="v1"><span begin="1.0" end="2.0">main</span></p>
          <p begin="1.5" end="3.0" ttm:role="x-bg"><span begin="1.5" end="2.5">echo</span></p>
          <p begin="4.0" end="6.0" ttm:agent="v2"><span begin="4.0" end="5.0">answer</span></p>
        </div></body></tt>"#;
        let (lines, _) = parse_ttml(xml).expect("ttml");
        assert_eq!(lines.len(), 2, "the x-bg line is merged, not a row");
        let Line::Words { bg, agent, .. } = &lines[0] else { panic!("words") };
        assert_eq!(agent.as_deref(), Some("v1"));
        assert_eq!(
            bg.as_ref().map(|words| words.iter().map(|word| word.x.as_str()).collect::<Vec<_>>()),
            Some(vec!["echo"])
        );
        let Line::Words { agent, .. } = &lines[1] else { panic!("words") };
        assert_eq!(agent.as_deref(), Some("v2"));
    }

    #[test]
    fn frames_and_empty_text_are_rejected() {
        assert_eq!(parse_time("00:00:10:12"), None);
        assert_eq!(parse_time("00:00:10.500"), Some(10.5));
        assert_eq!(parse_time("10.5s"), Some(10.5));
        assert_eq!(parse_time("500ms"), Some(0.5));
        assert_eq!(parse_ttml("<tt><body></body></tt>"), None);
    }
}
