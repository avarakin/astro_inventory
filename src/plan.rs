use std::collections::HashMap;
use std::sync::LazyLock;

/// Complete 88-constellation abbreviation map (from astro_inventory.py).
const CONSTELLATIONS: &[(&str, &str)] = &[
    ("And", "Andromeda"),
    ("Ant", "Antlia"),
    ("Aps", "Apus"),
    ("Ape", "Apus"),
    ("Aqr", "Aquarius"),
    ("Aql", "Aquila"),
    ("Ara", "Ara"),
    ("Ari", "Aries"),
    ("Aur", "Auriga"),
    ("Boo", "Bootes"),
    ("Cae", "Caelum"),
    ("Cam", "Camelopardalis"),
    ("Cap", "Capricornus"),
    ("Car", "Carina"),
    ("Cen", "Centaurus"),
    ("Cet", "Cetus"),
    ("Cha", "Chamaeleon"),
    ("Cir", "Circinus"),
    ("Col", "Columba"),
    ("Com", "Coma Berenices"),
    ("Crt", "Crater"),
    ("CVn", "Canes Venatici"),
    ("CMi", "Canis Minor"),
    ("CMa", "Canis Major"),
    ("Crt2", "Crater"),
    ("Cas", "Cassiopeia"),
    ("Cep", "Cepheus"),
    ("CrA", "Corona Australis"),
    ("CrB", "Corona Borealis"),
    ("Crv", "Corvus"),
    ("Cru", "Crux"),
    ("Cyg", "Cygnus"),
    ("Del", "Delphinus"),
    ("Dor", "Dorado"),
    ("Dra", "Draco"),
    ("Equ", "Equuleus"),
    ("Eri", "Eridanus"),
    ("For", "Fornax"),
    ("Gem", "Gemini"),
    ("Grus", "Grus"),
    ("Her", "Hercules"),
    ("Hor", "Horologium"),
    ("Hya", "Hydra"),
    ("Hyi", "Hydrus"),
    ("Ind", "Indus"),
    ("Lac", "Lacerta"),
    ("Leo", "Leo"),
    ("LMi", "Leo Minor"),
    ("Lep", "Lepus"),
    ("Lib", "Libra"),
    ("Lup", "Lupus"),
    ("Lyn", "Lynx"),
    ("Lyr", "Lyra"),
    ("Men", "Mensa"),
    ("Mic", "Microscopium"),
    ("Mon", "Monoceros"),
    ("Mus", "Musca"),
    ("Nor", "Norma"),
    ("Oct", "Octans"),
    ("Oph", "Ophiuchus"),
    ("Ori", "Orion"),
    ("Pav", "Pavo"),
    ("Peg", "Pegasus"),
    ("Per", "Perseus"),
    ("Phe", "Phoenix"),
    ("Pic", "Pictor"),
    ("Psc", "Pisces"),
    ("PsA", "Piscis Austrinus"),
    ("Pup", "Puppis"),
    ("Pyx", "Pyxis"),
    ("Rta", "Reticulum"),
    ("Sge", "Sagitta"),
    ("Sgr", "Sagittarius"),
    ("Sco", "Scorpius"),
    ("Scl", "Sculptor"),
    ("Sct", "Scutum"),
    ("Ser", "Serpens"),
    ("Sex", "Sextans"),
    ("Tau", "Taurus"),
    ("Tel", "Telescopium"),
    ("Tri", "Triangulum"),
    ("TrA", "Triangulum Australe"),
    ("Tuc", "Tucana"),
    ("UMi", "Ursa Minor"),
    ("UMa", "Ursa Major"),
    ("Vel", "Vela"),
    ("Vir", "Virgo"),
    ("Vul", "Vulpecula"),
    ("Vol", "Volans"),
];

/// Expand a constellation abbreviation to its full name.
/// Unknown values are returned unchanged.
pub fn expand_constellation(value: &str) -> String {
    if value.is_empty() {
        return value.to_string();
    }
    let trimmed = value.trim();
    for (abbr, full) in CONSTELLATIONS {
        if *abbr == trimmed {
            return full.to_string();
        }
    }
    value.to_string()
}

/// Parse plan.md text into (meta, body).
/// meta keys are lowercased. Returns ({}, text) when there is no YAML frontmatter.
pub fn parse_plan(text: &str) -> (HashMap<String, String>, String) {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.is_empty() || lines[0].trim() != "---" {
        return (HashMap::new(), text.to_string());
    }

    let mut meta = HashMap::new();
    for i in 1..lines.len() {
        if lines[i].trim() == "---" {
            let body: String = lines[i + 1..]
                .iter()
                .filter(|l| !l.trim_start().starts_with('#'))
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
                .join("\n");
            return (meta, body.trim().to_string());
        }
        if let Some(colon_idx) = lines[i].find(':') {
            let k = lines[i][..colon_idx].trim().to_lowercase();
            let v = lines[i][colon_idx + 1..].trim().to_string();
            meta.insert(k, v);
        }
    }
    (HashMap::new(), text.to_string())
}

/// Set a frontmatter key, returning the new plan text.
///
/// Updates the key in place when it exists (matching is case-insensitive, like
/// `parse_plan`), appends it to the existing frontmatter block otherwise, and
/// synthesises a `---` block at the top when the plan is plain text. The body is
/// never touched, so a hand-written plan keeps its notes and spacing.
///
/// Returns the input unchanged when the key already holds `value`, letting
/// callers skip the write.
pub fn set_meta(text: &str, key: &str, value: &str) -> String {
    let line = format!("{key}: {value}");
    let opens_with_frontmatter = text.find('\n').is_some_and(|i| text[..i].trim() == "---");
    if !opens_with_frontmatter {
        // No frontmatter block: prepend one.
        if text.trim().is_empty() {
            return format!("---\n{line}\n---\n");
        }
        return format!("---\n{line}\n---\n{text}");
    }

    let mut out = text.to_string();
    // Find the closing '---' and the key inside the block. Each newline marks
    // the start of the following line, and line 1 is the opening '---'.
    let mut close = None;
    let mut key_start = None;
    for (start, _) in text.match_indices('\n') {
        let line_end = text[start + 1..].find('\n').map(|n| start + 1 + n).unwrap_or(text.len());
        let line_text = &text[start + 1..line_end];
        if line_text.trim() == "---" {
            close = Some(start + 1);
            break;
        }
        if line_text.contains(':') && line_text[..line_text.find(':').unwrap()].trim().eq_ignore_ascii_case(key) {
            key_start = Some((start + 1, line_end));
        }
    }
    if let Some((s, e)) = key_start {
        if &text[s..e] == line.as_str() {
            return out;
        }
        out.replace_range(s..e, &line);
        return out;
    }
    match close {
        Some(c) => out.insert_str(c, &format!("{line}\n")),
        None => {
            // Unterminated frontmatter: append the key at the end of the block.
            out.push('\n');
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Does the plan text contain a TODO marker or section?
///
/// Plans in the wild put this on the first line (`TODO: take RGB`) and as a
/// markdown heading (`## TODO`); list bullets and emphasis are tolerated.
/// The marker must be a whole word, so `TODOS` or `TODOList` do not count.
pub fn has_todo_section(text: &str) -> bool {
    text.lines().any(|line| {
        // Drop a markdown heading prefix and any bullet/emphasis wrapper.
        let line = line.trim().trim_start_matches('#').trim();
        line.split(|c: char| !c.is_alphanumeric())
            .find(|word| !word.is_empty())
            .map_or(false, |word| word.eq_ignore_ascii_case("todo"))
    })
}

/// HTML-escape a string.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Wrap http(s) URLs in already-escaped text with target=_blank anchors.
static RE_URL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"https?://[^\s<>\x22']+").unwrap());

pub fn linkify(escaped_text: &str) -> String {
    RE_URL.replace_all(escaped_text, |caps: &regex::Captures| {
        format!(
            "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a>",
            caps.get(0).unwrap().as_str(),
            caps.get(0).unwrap().as_str()
        )
    })
    .to_string()
}

fn plan_link_label(url: &str) -> &'static str {
    if url.to_lowercase().contains("astrobin") {
        "Sample"
    } else {
        "Link"
    }
}

/// Render a plan.md string as a compact, styled HTML block.
pub fn render_plan(plan_text: &str) -> String {
    if plan_text.is_empty() || plan_text.trim().is_empty() {
        return "—".to_string();
    }

    let (meta, body) = parse_plan(plan_text);
    let mut top = Vec::new();

    if let Some(constellation) = meta.get("constellation") {
        if !constellation.is_empty() {
            let expanded = expand_constellation(constellation);
            top.push(format!(
                "<span class=\"badge\">{}</span>",
                html_escape(&expanded)
            ));
        }
    }
    if let Some(link) = meta.get("link") {
        if !link.is_empty() {
            let label = plan_link_label(link);
            top.push(format!(
                "<a class=\"plan-link\" href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{} &#8599;</a>",
                html_escape(link),
                html_escape(label)
            ));
        }
    }
    // `astrobin:` is written by the submit bridge, so it can sit next to a
    // hand-kept `link:` without either being overwritten.
    if let Some(link) = meta.get("astrobin") {
        if !link.is_empty() {
            top.push(format!(
                "<a class=\"plan-link\" href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">AstroBin &#8599;</a>",
                html_escape(link)
            ));
        }
    }

    let mut parts = Vec::new();
    if !top.is_empty() {
        parts.push(format!("<div class=\"plan-top\">{}</div>", top.join("")));
    }

    let mut meta_bits = Vec::new();
    if let Some(ra) = meta.get("ra") {
        if !ra.is_empty() {
            meta_bits.push(format!("RA {ra}"));
        }
    }
    if let Some(dec) = meta.get("dec") {
        if !dec.is_empty() {
            meta_bits.push(format!("Dec {dec}"));
        }
    }
    if let Some(rotation) = meta.get("rotation") {
        if !rotation.is_empty() {
            meta_bits.push(format!("Rot {rotation}"));
        }
    }
    if !meta_bits.is_empty() {
        let escaped: Vec<String> = meta_bits.iter().map(|b| html_escape(b)).collect();
        parts.push(format!(
            "<div class=\"plan-meta\">{}</div>",
            escaped.join("<br>")
        ));
    }

    if !body.is_empty() {
        let body_lines: Vec<String> = body
            .split('\n')
            .map(|l| linkify(&html_escape(l)))
            .collect();
        parts.push(format!(
            "<div class=\"plan-body\">{}</div>",
            body_lines.join("<br>")
        ));
    }

    if parts.is_empty() {
        return linkify(&html_escape(plan_text));
    }

    format!("<div class=\"plan\">{}</div>", parts.join(""))
}

#[cfg(test)]
mod tests {
    use super::has_todo_section;

    #[test]
    fn todo_markers_and_sections_are_detected() {
        for text in [
            "TODO: take RGB",
            "TODO",
            "## TODO\n- take RGB",
            "### todo: take RGB",
            "**TODO** take RGB",
            "- [ ] TODO: take RGB",
            "TODO:  Take RGB. Can be done from home\nRotation: 85",
        ] {
            assert!(has_todo_section(text), "missed TODO in {text:?}");
        }
    }

    #[test]
    fn set_meta_updates_in_place_and_keeps_the_body() {
        let plan = "---\nconstellation: Perseus\nlink: https://app.astrobin.com/i/aaaaa\n---\n\n# NGC_1333\n\n## Plan\nkeep the notes\n";
        let out = super::set_meta(plan, "astrobin", "https://app.astrobin.com/i/05feat");
        assert!(out.contains("astrobin: https://app.astrobin.com/i/05feat"));
        assert!(out.contains("link: https://app.astrobin.com/i/aaaaa"), "user link clobbered");
        assert!(out.contains("keep the notes"), "body changed");
        // Idempotent: same value returns the same text.
        assert_eq!(super::set_meta(&out, "astrobin", "https://app.astrobin.com/i/05feat"), out);
        // Re-submit replaces the previous draft link rather than adding a copy.
        let again = super::set_meta(&out, "astrobin", "https://app.astrobin.com/i/zzzzz");
        assert_eq!(again.matches("astrobin:").count(), 1);
        assert!(again.contains("i/zzzzz") && !again.contains("i/05feat"));
    }

    #[test]
    fn set_meta_creates_a_frontmatter_block_for_plain_plans() {
        let out = super::set_meta("TODO: take RGB\nRotation: 85\n", "astrobin", "https://app.astrobin.com/i/x");
        assert!(out.starts_with("---\nastrobin: https://app.astrobin.com/i/x\n---\nTODO: take RGB"));
        let empty = super::set_meta("", "astrobin", "https://app.astrobin.com/i/x");
        assert_eq!(empty, "---\nastrobin: https://app.astrobin.com/i/x\n---\n");
    }

    #[test]
    fn set_meta_matches_key_case_like_parse_plan() {
        let out = super::set_meta("---\nAstroBin: https://app.astrobin.com/i/old\n---\nbody\n", "astrobin", "https://app.astrobin.com/i/new");
        let keys = out
            .lines()
            .filter(|l| l.split(':').next().unwrap_or("").trim().eq_ignore_ascii_case("astrobin"))
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["astrobin: https://app.astrobin.com/i/new"]);
        assert!(!out.contains("i/old"));
    }

    #[test]
    fn ordinary_plan_text_is_not_a_todo() {
        for text in ["", "## Plan\nTake RGB", "TODOS are useful", "No TODO here", "TODOList"] {
            assert!(!has_todo_section(text), "false positive for {text:?}");
        }
    }
}
