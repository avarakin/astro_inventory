//! Extract the Deep Sky Compendium xlsx into `objects.json` + thumbnails.
//!
//! Usage: extract_compendium [input.xlsx] [output-dir]
//! Defaults: Compendium.xls, data/compendium
//!
//! The workbook is an xlsx container (despite the .xls name) whose thumbnails
//! are image fills on legacy VML comment shapes anchored to column A, so we
//! use calamine for cells + hyperlinks and zip + regex for the VML layer.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{bail, Context, Result};
use astro_inventory::compendium::{CompendiumObject, sanitize_name};
use calamine::{Data, Range, Reader, Xlsx};
use regex::Regex;

/// Data rows on the Main sheet, 1-based inclusive.
const FIRST_DATA_ROW: usize = 10;

/// Column indices (0-based) of the catalogue-ID columns AI..BI.
const CATALOGS: [(usize, &str); 27] = [
    (34, "NGC"),
    (35, "IC"),
    (36, "H400"),
    (37, "Messier"),
    (38, "Caldwell"),
    (39, "Arp"),
    (40, "Hickson"),
    (41, "Abell"),
    (42, "UGC"),
    (43, "PGC"),
    (44, "AbellPN"),
    (45, "Griffith"),
    (46, "Kohoutek"),
    (47, "Minkowski"),
    (48, "Barnard"),
    (49, "Gum"),
    (50, "LBN"),
    (51, "LDN"),
    (52, "RCW"),
    (53, "Sh2"),
    (54, "SNR"),
    (55, "vdB"),
    (56, "HT"),
    (57, "SD"),
    (58, "OB"),
    (59, "SP"),
    (60, "FG"),
];

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| "Compendium.xls".to_string());
    let outdir = args.next().unwrap_or_else(|| "data/compendium".to_string());

    eprintln!("[extract] input: {input}  out: {outdir}");

    // --- cells + hyperlinks (calamine; hard-link to .xlsx because calamine
    // dispatches on file extension and our file is named .xls) ---
    let tmp_xlsx = format!("{input}.tmp.xlsx");
    if std::fs::hard_link(&input, &tmp_xlsx).is_err() {
        std::fs::copy(&input, &tmp_xlsx).with_context(|| format!("cannot stage {tmp_xlsx}"))?;
    }
    let opened: Result<Xlsx<_>, _> = calamine::open_workbook(&tmp_xlsx);
    let _ = std::fs::remove_file(&tmp_xlsx);
    let mut wb = opened.context("failed to parse workbook as xlsx")?;
    let hyperlinks = wb
        .hyperlinks_by_sheet_name("Main")
        .unwrap_or_default();
    let range: Range<Data> = wb.worksheet_range("Main")?;

    // --- thumbnails: VML comment shapes with image fills, anchored to column A ---
    let thumbs = extract_thumb_mapping(&input)?;

    let thumbs_dir = Path::new(&outdir).join("thumbs");
    std::fs::create_dir_all(&thumbs_dir).context("cannot create thumbs dir")?;

    // --- rows -> records ---
    let mut objects: Vec<CompendiumObject> = Vec::new();
    let mut media_zip = zip::ZipArchive::new(std::fs::File::open(&input).context("reopen input")?)
        .context("input is not a zip (xlsx) container")?;

    for r in (FIRST_DATA_ROW - 1)..range.height() {
        let cell = |c: usize| -> Option<String> { cell_str(range.get((r, c))) };
        let cellf = |c: usize| -> Option<f64> { cell_f64(range.get((r, c))) };
        let link = |c: usize| -> Option<String> {
            hyperlinks
                .iter()
                .find(|h| h.contains(r as u32, c as u32))
                .and_then(|h| h.target.clone())
                .filter(|t| t.starts_with("http"))
        };

        let Some(name) = cell(0) else { continue }; // column A: object name
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }

        let id = (r - (FIRST_DATA_ROW - 1) + 1) as u32;

        let obj_type = cell(3).unwrap_or_default().trim().to_string();
        let subtype = cell(4).unwrap_or_default().trim().to_string();

        let mut catalogs = BTreeMap::new();
        for &(col, cat) in &CATALOGS {
            if let Some(v) = cell(col) {
                let v = v.trim().to_string();
                if !v.is_empty() {
                    catalogs.insert(cat.to_string(), v);
                }
            }
        }

        // Thumbnail: VML <x:Row> is 0-based, so sheet row FIRST_DATA_ROW -> x:Row FIRST_DATA_ROW-1.
        let thumb = thumbs
            .get(&r)
            .map(|(_, _)| format!("{:04}.{}.jpg", id, sanitize_name(&name)));

        if let Some((_, media_path)) = thumbs.get(&r) {
            let bytes = read_from_archive(&mut media_zip, media_path)
                .with_context(|| format!("media {media_path} for {name}"))?;
            let fname = thumb.as_ref().unwrap();
            std::fs::write(thumbs_dir.join(fname), bytes)
                .with_context(|| format!("write thumb {fname}"))?;
        }

        objects.push(CompendiumObject {
            id,
            name,
            obj_type,
            subtype,
            class: cell(5),
            size_arcmin: cellf(6),
            distance: cellf(7),
            diameter: cellf(8),
            rating: cellf(9),
            notes: cell(10),
            ra_hms: cell(11),
            ra_deg: cellf(12),
            dec_dms: cell(13),
            dec_deg: cellf(14),
            constellation: cell(15),
            nickname: cell(16),
            alt_id: cell(17),
            nearby: cell(18),
            visual_mag: cellf(19),
            surf_brightness: cellf(20),
            inclination: cellf(21),
            catalogs,
            simbad_key: cell(62),
            aladin_fov: cell(63),
            filters: cell(65),
            astrobin_url: link(0),
            simbad_url: link(1),
            aladin_url: link(2),
            thumb,
        });
    }

    let json_path = Path::new(&outdir).join("objects.json");
    let json = serde_json::to_vec_pretty(&objects).context("serialize")?;
    std::fs::write(&json_path, json).with_context(|| format!("write {}", json_path.display()))?;

    let n_thumbs = objects.iter().filter(|o| o.thumb.is_some()).count();
    eprintln!(
        "[extract] wrote {} objects ({} with thumbnails) to {}",
        objects.len(),
        n_thumbs,
        json_path.display()
    );
    if objects.len() < 3000 {
        bail!("suspiciously few objects extracted ({}); check sheet/columns", objects.len());
    }
    Ok(())
}

/// Map sheet row (0-based) -> (shape id, media zip path) for column-A image fills.
fn extract_thumb_mapping(input: &str) -> Result<HashMap<usize, (String, String)>> {
    let rels_xml = read_zip_entry(input, "xl/drawings/_rels/vmlDrawing1.vml.rels")?;
    let vml_xml = read_zip_entry(input, "xl/drawings/vmlDrawing1.vml")?;
    let rels_xml = String::from_utf8_lossy(&rels_xml).to_string();
    let vml_xml = String::from_utf8_lossy(&vml_xml).to_string();

    // rId -> "xl/media/imageN.jpeg"
    let rel_re = Regex::new(r#"Id="([^"]+)"[^>]*Target="([^"]+)""#)?;
    let mut rid_to_media = HashMap::new();
    for cap in rel_re.captures_iter(&rels_xml) {
        let target = cap[2].replace("../", "xl/");
        rid_to_media.insert(cap[1].to_string(), target);
    }

    let shape_re = Regex::new(r"(?s)<v:shape\b.*?</v:shape>")?;
    let relid_re = Regex::new(r#"o:relid="([^"]+)""#)?;
    let row_re = Regex::new(r"<x:Row>(\d+)</x:Row>")?;
    let col_re = Regex::new(r"<x:Column>(\d+)</x:Column>")?;

    let mut map = HashMap::new();
    for shape in shape_re.find_iter(&vml_xml) {
        let s = &vml_xml[shape.range()];
        if !s.contains("type=\"frame\"") {
            continue; // image fill only
        }
        let Some(col_m) = col_re.captures(s) else { continue };
        if &col_m[1] != "0" {
            continue; // thumbnails live in column A
        }
        let Some(rel_m) = relid_re.captures(s) else { continue };
        let Some(row_m) = row_re.captures(s) else { continue };
        let row: usize = row_m[1].parse()?;
        if let Some(media) = rid_to_media.get(&rel_m[1]) {
            map.insert(row, (rel_m[1].to_string(), media.clone()));
        }
    }
    Ok(map)
}

fn read_zip_entry(input: &str, path: &str) -> Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(input)?)
        .context("input is not a zip (xlsx) container")?;
    read_from_archive(&mut zip, path)
}

fn read_from_archive<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    path: &str,
) -> Result<Vec<u8>> {
    let mut entry = zip
        .by_name(path)
        .with_context(|| format!("zip entry not found: {path}"))?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf)?;
    Ok(buf)
}

fn cell_str(d: Option<&Data>) -> Option<String> {
    match d? {
        Data::Empty | Data::Error(_) => None,
        Data::String(s) => {
            let t = s.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        }
        Data::Float(f) => Some(fmt_num(*f)),
        Data::Int(i) => Some(i.to_string()),
        Data::Bool(b) => Some(if *b { "Y".into() } else { String::new() }),
        _ => None,
    }
}

fn cell_f64(d: Option<&Data>) -> Option<f64> {
    match d? {
        Data::Float(f) => Some(*f),
        Data::Int(i) => Some(*i as f64),
        Data::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn fmt_num(f: f64) -> String {
    if f == f.trunc() {
        format!("{}", f as i64)
    } else {
        f.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_num_strips_integral_floats() {
        assert_eq!(fmt_num(3.0), "3");
        assert_eq!(fmt_num(3.15), "3.15");
    }

    #[test]
    fn cell_helpers_parse_mixed_types() {
        assert_eq!(cell_str(Some(&Data::String("  Cep ".into()))).as_deref(), Some("Cep"));
        assert_eq!(cell_str(Some(&Data::String("   ".into()))), None);
        assert_eq!(cell_str(Some(&Data::Empty)), None);
        assert_eq!(cell_f64(Some(&Data::String("0.8".into()))), Some(0.8));
        assert_eq!(cell_f64(Some(&Data::Int(13000))), Some(13000.0));
    }
}
