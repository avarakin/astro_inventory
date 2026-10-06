# Re-extract the Compendium catalog only when inputs are newer than output.
.PHONY: compendium

compendium: data/compendium/objects.json

data/compendium/objects.json: Compendium.xls src/bin/extract_compendium.rs src/compendium.rs Cargo.toml
	cargo run --release --bin extract_compendium -- Compendium.xls data/compendium
