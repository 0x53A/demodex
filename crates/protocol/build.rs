use quote::ToTokens;
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn attrs(attributes: &mut Vec<syn::Attribute>) {
    attributes.retain(|a| !a.path().is_ident("doc") && !a.path().is_ident("derive"));
}

fn schema(source: &str) -> String {
    let mut file = syn::parse_file(source).expect("parse protocol declarations");
    file.items.retain(|item| {
        matches!(
            item,
            syn::Item::Enum(_) | syn::Item::Struct(_) | syn::Item::Type(_)
        )
    });
    for item in &mut file.items {
        match item {
            syn::Item::Enum(item) => {
                attrs(&mut item.attrs);
                for variant in &mut item.variants {
                    attrs(&mut variant.attrs);
                    for field in &mut variant.fields {
                        attrs(&mut field.attrs);
                    }
                }
            }
            syn::Item::Struct(item) => {
                attrs(&mut item.attrs);
                for field in &mut item.fields {
                    attrs(&mut field.attrs);
                }
            }
            syn::Item::Type(item) => attrs(&mut item.attrs),
            _ => unreachable!(),
        }
    }
    file.into_token_stream().to_string()
}

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=src/wire.rs");
    println!("cargo:rerun-if-changed=../../Cargo.lock");
    let source = fs::read_to_string(root.join("src/wire.rs")).unwrap();
    let lock = fs::read_to_string(root.join("../../Cargo.lock")).unwrap();
    let mut digest = Sha256::new();
    digest.update(b"demodex-schema-v1\n");
    digest.update(schema(&source));
    // Transport serialization can change even if our own message enums do not.
    // Include every locked version/source of these packages, but not unrelated
    // UI/backend dependencies, to permit independent compatible releases.
    for package in lock.split("[[package]]").skip(1) {
        if [
            "ractor",
            "ractor_wormhole",
            "ractor_wormhole_derive",
            "bincode",
            "bincode_derive",
        ]
        .iter()
        .any(|name| {
            package
                .lines()
                .any(|line| line == format!("name = \"{name}\""))
        }) {
            for line in package.lines().filter(|line| {
                line.starts_with("name = ")
                    || line.starts_with("version = ")
                    || line.starts_with("source = ")
                    || line.starts_with("checksum = ")
            }) {
                digest.update(line);
                digest.update(b"\n");
            }
        }
    }
    let hash: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    println!("cargo:rustc-env=DEMODEX_PROTOCOL_SCHEMA={hash}");
    // Guard canonicalization: formatting/docs don't invalidate a release;
    // changing fields or variant order must change the fingerprint.
    assert_eq!(
        schema("enum M { A { x: u32 } }"),
        schema("/// docs\n enum M{A{x:u32}}")
    );
    assert_ne!(
        schema("enum M { A { x: u32 } }"),
        schema("enum M { A { x: u64 } }")
    );
    assert_ne!(schema("enum M { A, B }"), schema("enum M { B, A }"));
}
