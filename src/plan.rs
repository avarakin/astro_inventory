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
        "AstroBin"
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
    fn ordinary_plan_text_is_not_a_todo() {
        for text in ["", "## Plan\nTake RGB", "TODOS are useful", "No TODO here", "TODOList"] {
            assert!(!has_todo_section(text), "false positive for {text:?}");
        }
    }
}
