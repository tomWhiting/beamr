use beamr::atom::AtomTable;
use beamr::loader::{archive_pack, load_module_with_origin};
use beamr::module::{ModuleOrigin, ModuleRegistry};
use beamr::native::BifRegistryImpl;

#[test]
fn decompressed_archive_fixture_loads_with_embedded_origin() {
    let fixture_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let archive_bytes = archive_pack(&fixture_dir).expect("fixture archive packs");
    let archive = beamr::loader::EmbeddedArchive::parse(&archive_bytes).expect("archive parses");
    let bytes = archive.get("hello").expect("hello fixture embedded");

    let atoms = AtomTable::with_common_atoms();
    let registry = ModuleRegistry::new();
    let bifs = BifRegistryImpl::new();
    let (module, _report) =
        load_module_with_origin(&bytes, &atoms, &registry, &bifs, ModuleOrigin::Embedded)
            .expect("embedded fixture loads through standard loader");

    assert_eq!(module.origin, ModuleOrigin::Embedded);
    assert_eq!(registry.origin(module.name), Some(ModuleOrigin::Embedded));
}
