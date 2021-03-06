use std::env;
use std::fs::{read_dir, read_to_string, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("targets.rs");
    let mut f = File::create(&dest_path).unwrap();

    // Determine all config files to parse.
    let mut files = vec![];
    visit_dirs(Path::new("targets"), &mut files).unwrap();

    let mut configs: Vec<proc_macro2::TokenStream> = vec![];
    for file in files {
        let string = read_to_string(&file).expect(
            "Algorithm definition file could not be read. This is a bug. Please report it.",
        );

        let yaml: Result<serde_yaml::Value, _> = serde_yaml::from_str(&string);

        match yaml {
            Ok(chip) => {
                let chip = extract_chip_family(&chip);
                configs.push(chip);
            }
            Err(e) => {
                panic!("Failed to parse target file: {:?} because:\n{}", file, e);
            }
        }
    }

    let stream: String = format!(
        "{}",
        quote::quote! {
            vec![
                #(#configs,)*
            ]
        }
    );

    f.write_all(stream.as_bytes())
        .expect("Writing build.rs output failed.");
}

// one possible implementation of walking a directory only visiting files
fn visit_dirs(dir: &Path, targets: &mut Vec<PathBuf>) -> io::Result<()> {
    if dir.is_dir() {
        for entry in read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, targets)?;
            } else {
                targets.push(path.to_owned());
            }
        }
    }
    Ok(())
}

/// Creates a properly quoted Option<T>` `TokenStream` from an `Option<T>`.
fn quote_option<T: quote::ToTokens>(option: Option<T>) -> proc_macro2::TokenStream {
    if let Some(value) = option {
        quote::quote! {
            Some(#value)
        }
    } else {
        quote::quote! {
            None
        }
    }
}

/// Extracts a list of algorithm token streams from a yaml value.
fn extract_algorithms(chip: &serde_yaml::Value) -> Vec<proc_macro2::TokenStream> {
    // Get an iterator over all the algorithms contained in the chip value obtained from the yaml file.
    let algorithm_iter = chip
        .get("flash_algorithms")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter();

    algorithm_iter
        .map(|algorithm| {
            // Extract all values and form them into a struct.
            let name = algorithm
                .get("name")
                .unwrap()
                .as_str()
                .unwrap()
                .to_ascii_lowercase();
            let description = algorithm
                .get("description")
                .unwrap()
                .as_str()
                .unwrap()
                .to_ascii_lowercase();
            let default = algorithm.get("default").unwrap().as_bool().unwrap();
            let instructions = algorithm
                .get("instructions")
                .unwrap()
                .as_sequence()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u32);
            let pc_init =
                quote_option(algorithm.get("pc_init").unwrap().as_u64().map(|v| v as u32));
            let pc_uninit = quote_option(
                algorithm
                    .get("pc_uninit")
                    .unwrap()
                    .as_u64()
                    .map(|v| v as u32),
            );
            let pc_program_page =
                algorithm.get("pc_program_page").unwrap().as_u64().unwrap() as u32;
            let pc_erase_sector =
                algorithm.get("pc_erase_sector").unwrap().as_u64().unwrap() as u32;
            let pc_erase_all = quote_option(
                algorithm
                    .get("pc_erase_all")
                    .unwrap()
                    .as_u64()
                    .map(|v| v as u32),
            );
            let data_section_offset = algorithm
                .get("data_section_offset")
                .unwrap()
                .as_u64()
                .unwrap() as u32;

            // Quote the algorithm struct.
            let algorithm = quote::quote! {
                RawFlashAlgorithm {
                    name: #name.to_owned(),
                    description: #description.to_owned(),
                    default: #default,
                    instructions: vec![
                        #(#instructions,)*
                    ],
                    pc_init: #pc_init,
                    pc_uninit: #pc_uninit,
                    pc_program_page: #pc_program_page,
                    pc_erase_sector: #pc_erase_sector,
                    pc_erase_all: #pc_erase_all,
                    data_section_offset: #data_section_offset,
                }
            };

            algorithm
        })
        .collect()
}

/// Extracts a list of algorithm token streams from a yaml value.
fn extract_memory_map(chip: &serde_yaml::Value) -> Vec<proc_macro2::TokenStream> {
    // Get an iterator over all the algorithms contained in the chip value obtained from the yaml file.
    let memory_map_iter = chip
        .get("memory_map")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter();

    memory_map_iter
        .filter_map(|memory_region| {
            // Check if it's a RAM region. If yes, parse it into a TokenStream.
            memory_region
                .get("Ram")
                .map(|region| {
                    let range = region.get("range").unwrap();
                    let start = range.get("start").unwrap().as_u64().unwrap() as u32;
                    let end = range.get("end").unwrap().as_u64().unwrap() as u32;
                    let is_boot_memory = region.get("is_boot_memory").unwrap().as_bool().unwrap();

                    quote::quote! {
                        MemoryRegion::Ram(RamRegion {
                            range: #start..#end,
                            is_boot_memory: #is_boot_memory,
                        })
                    }
                })
                .or_else(|| {
                    memory_region.get("Flash").map(|region| {
                        let range = region.get("range").unwrap();
                        let start = range.get("start").unwrap().as_u64().unwrap() as u32;
                        let end = range.get("end").unwrap().as_u64().unwrap() as u32;
                        let is_boot_memory =
                            region.get("is_boot_memory").unwrap().as_bool().unwrap();
                        let sector_size =
                            region.get("sector_size").unwrap().as_u64().unwrap() as u32;
                        let page_size = region.get("page_size").unwrap().as_u64().unwrap() as u32;
                        let erased_byte_value =
                            region.get("erased_byte_value").unwrap().as_u64().unwrap() as u8;

                        quote::quote! {
                            MemoryRegion::Flash(FlashRegion {
                                range: #start..#end,
                                is_boot_memory: #is_boot_memory,
                                sector_size: #sector_size,
                                page_size: #page_size,
                                erased_byte_value: #erased_byte_value,
                            })
                        }
                    })
                })
        })
        .collect()
}

/// Extracts a list of algorithm token streams from a yaml value.
fn extract_variants(chip_family: &serde_yaml::Value) -> Vec<proc_macro2::TokenStream> {
    // Get an iterator over all the algorithms contained in the chip value obtained from the yaml file.
    let variants_iter = chip_family
        .get("variants")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter();

    variants_iter
        .map(|variant| {
            let name = variant.get("name").unwrap().as_str().unwrap();
            let part = quote_option(
                variant
                    .get("part")
                    .and_then(|v| v.as_u64().map(|v| v as u16)),
            );

            // Extract all the memory regions into a Vec of TookenStreams.
            let memory_map = extract_memory_map(&variant);

            quote::quote! {
                Chip {
                    name: #name.to_owned(),
                    part: #part,
                    memory_map: vec![
                        #(#memory_map,)*
                    ],
                }
            }
        })
        .collect()
}

/// Extracts a chip family token stream from a yaml value.
fn extract_chip_family(chip_family: &serde_yaml::Value) -> proc_macro2::TokenStream {
    // Extract all the algorithms into a Vec of TokenStreams.
    let algorithms = extract_algorithms(&chip_family);

    // Extract all the available variants into a Vec of TokenStreams.
    let variants = extract_variants(&chip_family);

    let name = chip_family
        .get("name")
        .unwrap()
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    let core = chip_family
        .get("core")
        .unwrap()
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    let manufacturer = quote_option(extract_manufacturer(&chip_family));

    // Quote the chip.
    let chip_family = quote::quote! {
        ChipFamily {
            name: #name.to_owned(),
            manufacturer: #manufacturer,
            flash_algorithms: vec![
                #(#algorithms,)*
            ],
            variants: vec![
                #(#variants,)*
            ],
            core: #core.to_owned(),
        }
    };

    chip_family
}

/// Extracts the jep code token stream from a yaml value.
fn extract_manufacturer(chip: &serde_yaml::Value) -> Option<proc_macro2::TokenStream> {
    chip.get("manufacturer").map(|manufacturer| {
        let cc = manufacturer.get("cc").map(|v| v.as_u64().unwrap() as u8);
        let id = manufacturer.get("id").map(|v| v.as_u64().unwrap() as u8);

        quote::quote! {
            JEP106Code {
                cc: #cc,
                id: #id,
            }
        }
    })
}
