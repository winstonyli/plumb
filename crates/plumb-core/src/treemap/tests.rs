use super::*;

fn class_of(name: &str) -> Class {
    classify(&name.encode_utf16().collect::<Vec<u16>>())
}

#[test]
fn classification_by_last_extension_ignores_case_and_defaults_to_other() {
    for (name, want) in [
        ("movie.MP4", Class::Video),
        ("song.flac", Class::Audio),
        ("IMG_1.JpG", Class::Image),
        ("a.tar.gz", Class::Archive),
        ("report.docx", Class::Document),
        ("lib.rs", Class::Code),
        ("setup.exe", Class::Executable),
        ("disk.vhdx", Class::DiskImage),
        ("data.sqlite3", Class::Database),
        ("noext", Class::Other),
        ("trailing.", Class::Other),
        (".hidden", Class::Other),
        ("x.y.", Class::Other),
        ("weird.\u{fc}n\u{ef}", Class::Other),
        ("a.verylongextension", Class::Other),
    ] {
        assert_eq!(class_of(name), want, "{name}");
    }
}

#[test]
fn all_lists_every_class_in_index_order() {
    for (i, c) in Class::ALL.iter().enumerate() {
        assert_eq!(c.index(), i);
        assert!(!c.label().is_empty());
    }
}
