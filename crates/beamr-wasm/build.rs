use std::env;
use std::fmt::Write;
use std::fs;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};

const MAGIC: &[u8] = b"BEAMR_EMBED\0";
const VERSION: u8 = 1;
const HEADER_LEN: usize = MAGIC.len() + 1 + 4;
const ZSTD_LEVEL: i32 = 3;
const DEFAULT_WASM_BINDGEN_IMPORT: &str = "./beamr_wasm.js";

struct BeamModule {
    name: String,
    path: PathBuf,
    bytes: Vec<u8>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=BEAMR_WASM_BUNDLE_DIR");
    println!("cargo:rerun-if-env-changed=BEAMR_EMBED_DIR");
    println!("cargo:rerun-if-env-changed=BEAMR_WASM_BINDGEN_IMPORT");

    let out_dir = env::var_os("OUT_DIR")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OUT_DIR is not set"))?;
    let bundle_dir = Path::new(&out_dir).join("beamr-wasm-bundle");
    fs::create_dir_all(&bundle_dir)?;

    let modules = match bundle_source_dir() {
        Some(dir) => {
            println!("cargo:rerun-if-changed={}", dir.display());
            collect_modules(&dir)?
        }
        None => Vec::new(),
    };

    let archive = archive_pack(&modules)?;
    fs::write(bundle_dir.join("modules.bin"), archive)?;
    fs::write(bundle_dir.join("manifest.json"), manifest_json(&modules))?;
    fs::write(bundle_dir.join("bootstrap.js"), bootstrap_js(&modules)?)?;
    fs::write(bundle_dir.join("package-bundle.mjs"), package_script())?;

    Ok(())
}

fn bundle_source_dir() -> Option<PathBuf> {
    env::var_os("BEAMR_WASM_BUNDLE_DIR")
        .or_else(|| env::var_os("BEAMR_EMBED_DIR"))
        .map(PathBuf::from)
}

fn collect_modules(beam_dir: &Path) -> Result<Vec<BeamModule>, Box<dyn std::error::Error>> {
    let mut modules = Vec::new();
    for entry in fs::read_dir(beam_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("beam") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(&path)?;
        modules.push(BeamModule {
            name: name.to_owned(),
            path,
            bytes,
        });
    }
    modules.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(modules)
}

fn archive_pack(modules: &[BeamModule]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut packed = empty_archive_bytes();
    let count = u32::try_from(modules.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("too many embedded modules ({})", modules.len()),
        )
    })?;
    packed[MAGIC.len() + 1..HEADER_LEN].copy_from_slice(&count.to_le_bytes());

    for module in modules {
        let compressed = zstd::stream::encode_all(Cursor::new(&module.bytes), ZSTD_LEVEL)?;
        write_entry(&mut packed, &module.name, &compressed)?;
    }

    Ok(packed)
}

fn empty_archive_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes
}

fn write_entry(output: &mut Vec<u8>, name: &str, compressed: &[u8]) -> io::Result<()> {
    let name_len = u16::try_from(name.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("embedded module name {name:?} is too long"),
        )
    })?;
    let beam_len = u32::try_from(compressed.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("embedded module {name:?} compressed payload is too long"),
        )
    })?;
    output.extend_from_slice(&name_len.to_le_bytes());
    output.extend_from_slice(name.as_bytes());
    output.extend_from_slice(&beam_len.to_le_bytes());
    output.extend_from_slice(compressed);
    Ok(())
}

fn manifest_json(modules: &[BeamModule]) -> String {
    let mut output = String::from(
        "{\n  \"format\": \"beamr-wasm-bundle\",\n  \"version\": 1,\n  \"wasm\": \"beamr_wasm_bg.wasm\",\n  \"bootstrap\": \"bootstrap.js\",\n  \"archive\": \"modules.bin\",\n  \"modules\": [",
    );
    for (index, module) in modules.iter().enumerate() {
        if index == 0 {
            output.push('\n');
        } else {
            output.push_str(",\n");
        }
        output.push_str("    { \"name\": ");
        push_json_string(&mut output, &module.name);
        output.push_str(", \"file\": ");
        let file = module
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .map_or("<unknown>", |name| name);
        push_json_string(&mut output, file);
        output.push_str(", \"bytes\": ");
        output.push_str(&module.bytes.len().to_string());
        output.push_str(" }");
    }
    output.push_str("\n  ]\n}\n");
    output
}

fn bootstrap_js(modules: &[BeamModule]) -> Result<String, Box<dyn std::error::Error>> {
    let wasm_import = env::var("BEAMR_WASM_BINDGEN_IMPORT")
        .unwrap_or_else(|_| DEFAULT_WASM_BINDGEN_IMPORT.to_owned());
    let mut output = String::new();
    writeln!(
        output,
        "// Generated by crates/beamr-wasm/build.rs; do not edit."
    )?;
    writeln!(
        output,
        "import init, {{ create_vm, WasmVm }} from {};",
        js_string(&wasm_import)
    )?;
    writeln!(output)?;
    writeln!(output, "const MODULES = [")?;
    for module in modules {
        writeln!(
            output,
            "  {{ name: {}, base64: {} }},",
            js_string(&module.name),
            js_string(&base64_encode(&module.bytes))
        )?;
    }
    writeln!(output, "];\n")?;
    output.push_str(BOOTSTRAP_RUNTIME);
    Ok(output)
}

fn js_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                output.push_str("\\u");
                output.push_str(&format!("{:04x}", character as u32));
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn push_json_string(output: &mut String, value: &str) {
    output.push_str(&js_string(value));
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(b0 >> 2) as usize] as char);
        output.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn package_script() -> &'static str {
    PACKAGE_SCRIPT
}

const BOOTSTRAP_RUNTIME: &str = r#"function decodeBase64(base64) {
  if (typeof atob === "function") {
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return bytes;
  }
  if (typeof Buffer === "function") {
    return new Uint8Array(Buffer.from(base64, "base64"));
  }
  throw new Error("No base64 decoder is available in this JavaScript host");
}

function parseJsonResult(value) {
  if (typeof value === "string") {
    return JSON.parse(value);
  }
  return value;
}

export { WasmVm };

export function bundledModules() {
  return MODULES.map((module) => ({ name: module.name, bytes: decodeBase64(module.base64) }));
}

export async function initBeamr(wasmPathOrModule) {
  await init(wasmPathOrModule);
  return { createVm: createPreloadedVm, modules: MODULES.map((module) => module.name) };
}

export async function createPreloadedVm(wasmPathOrModule) {
  await init(wasmPathOrModule);
  const vm = create_vm();
  const loads = [];
  for (const module of MODULES) {
    const result = parseJsonResult(vm.load_module(decodeBase64(module.base64)));
    loads.push(result);
  }
  return { vm, loads };
}

export async function spawnPreloaded(module, functionName, args = [], options = {}) {
  const { vm, loads } = await createPreloadedVm(options.wasmPathOrModule);
  const pid = vm.spawn(module, functionName, JSON.stringify(args));
  return { vm, pid, loads };
}

export function runUntilExit(vm, pid, options = {}) {
  const maxSteps = options.maxSteps ?? 1024;
  for (let step = 0; step < maxSteps; step += 1) {
    const summary = parseJsonResult(vm.run_step());
    const result = summary.results.find((entry) => entry.pid === pid);
    if (result) {
      return { summary, result };
    }
    if (summary.executed === 0 && summary.waiting === 0 && summary.yielded === 0) {
      return { summary, result: undefined };
    }
  }
  throw new Error(`BEAM process ${pid} did not exit within the ${maxSteps} step limit`);
}

const api = { WasmVm, initBeamr, createPreloadedVm, spawnPreloaded, runUntilExit, bundledModules };
if (typeof globalThis !== "undefined") {
  globalThis.BeamrBundle = api;
}

export default api;
"#;

const PACKAGE_SCRIPT: &str = r#"#!/usr/bin/env node
import { readFile, writeFile } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

function usage() {
  return "usage: node package-bundle.mjs <wasm-pack-pkg-dir> [output-file]";
}

function jsString(value) {
  return JSON.stringify(value).replace(/</g, "\\u003c");
}

async function main() {
  const pkgDir = process.argv[2];
  if (!pkgDir) {
    throw new Error(usage());
  }
  const output = process.argv[3] ?? join(pkgDir, "beamr.bundle.mjs");
  const bootstrapPath = join(dirname(fileURLToPath(import.meta.url)), "bootstrap.js");
  const bootstrap = await readFile(bootstrapPath, "utf8");
  const gluePath = join(pkgDir, "beamr_wasm.js");
  const wasmPath = join(pkgDir, "beamr_wasm_bg.wasm");
  const glue = await readFile(gluePath, "utf8");
  const wasm = await readFile(wasmPath);
  const wasmBase64 = wasm.toString("base64");
  const rewrittenBootstrap = bootstrap.replace(
    'import init, { create_vm, WasmVm } from "./beamr_wasm.js";',
    'const { default: init, create_vm, WasmVm } = await importWasmBindgen();'
  );
  const bundle = `// Generated Beamr single-file WASM bundle.\nconst WASM_BASE64 = ${jsString(wasmBase64)};\nconst WASM_GLUE = ${jsString(glue)};\n\nfunction decodeEmbeddedBase64(base64) {\n  if (typeof atob === "function") {\n    const binary = atob(base64);\n    const bytes = new Uint8Array(binary.length);\n    for (let index = 0; index < binary.length; index += 1) {\n      bytes[index] = binary.charCodeAt(index);\n    }\n    return bytes;\n  }\n  if (typeof Buffer === "function") {\n    return new Uint8Array(Buffer.from(base64, "base64"));\n  }\n  throw new Error("No base64 decoder is available in this JavaScript host");\n}\n\nasync function importWasmBindgen() {\n  const source = WASM_GLUE.replace(/new URL\\(['\"]beamr_wasm_bg\\.wasm['\"], import\\.meta\\.url\\)/g, "decodeEmbeddedBase64(WASM_BASE64)");\n  const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));\n  try {\n    return await import(url);\n  } finally {\n    URL.revokeObjectURL(url);\n  }\n}\n\n${rewrittenBootstrap}\n`;
  await writeFile(output, bundle);
  console.log(`wrote ${basename(output)}`);
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
});
"#;
