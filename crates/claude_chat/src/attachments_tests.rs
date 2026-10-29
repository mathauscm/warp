use super::*;

#[test]
fn classifies_by_extension() {
    let cases = [
        ("/a/photo.PNG", false, AttachmentKind::Image),
        ("/a/paper.pdf", false, AttachmentKind::Pdf),
        ("/a/page.html", false, AttachmentKind::Text),
        ("/a/main.rs", false, AttachmentKind::Text),
        ("/a/report.docx", false, AttachmentKind::Document),
        ("/a/data.xlsx", false, AttachmentKind::Spreadsheet),
        ("/a/data.csv", false, AttachmentKind::Spreadsheet),
        ("/a/deck.pptx", false, AttachmentKind::Presentation),
        ("/a/archive.zip", false, AttachmentKind::Other),
        ("/a/Makefile", false, AttachmentKind::Other),
        ("/a/src", true, AttachmentKind::Folder),
    ];
    for (path, is_dir, kind) in cases {
        assert_eq!(classify(path, is_dir), kind, "{path}");
    }
    assert!(has_text_extension("/a/data.csv"));
    assert!(!has_text_extension("/a/data.xlsx"));
    assert_eq!(image_media_type("/a/x.JPG"), Some("image/jpeg"));
    assert_eq!(image_media_type("/a/x.pdf"), None);
}

#[test]
fn sniffs_text() {
    assert!(looks_like_text(b"FROM rust:1.80\nRUN cargo build\n"));
    assert!(looks_like_text("ação".as_bytes()));
    // A multi-byte character cut by the end of the sniffed bytes.
    assert!(looks_like_text(&"ação".as_bytes()[..2]));
    assert!(!looks_like_text(b"PK\x03\x04\x00\x01"));
    assert!(!looks_like_text(&[0xff, 0xfe, 0x41, 0x42]));
}

#[test]
fn picks_delivery() {
    let big = MAX_INLINE_BYTES + 1;
    let cases = [
        (AttachmentKind::Image, 10, false, FileDelivery::Image),
        (AttachmentKind::Image, big, false, FileDelivery::Path),
        (AttachmentKind::Pdf, 10, false, FileDelivery::Document),
        (AttachmentKind::Pdf, big, false, FileDelivery::Path),
        (AttachmentKind::Text, big, true, FileDelivery::Mention),
        (AttachmentKind::Spreadsheet, 10, true, FileDelivery::Mention),
        (AttachmentKind::Spreadsheet, 10, false, FileDelivery::Path),
        (AttachmentKind::Document, 10, false, FileDelivery::Path),
        (AttachmentKind::Presentation, 10, false, FileDelivery::Path),
        (AttachmentKind::Other, 10, true, FileDelivery::Mention),
        (AttachmentKind::Other, 10, false, FileDelivery::Path),
        (AttachmentKind::Folder, 0, false, FileDelivery::Mention),
    ];
    for (kind, size, is_text, expected) in cases {
        assert_eq!(delivery(kind, size, is_text), expected, "{kind:?} {size}");
    }
}

fn listed(path: &str, is_dir: bool, is_mention: bool) -> ListedFile {
    ListedFile {
        path: path.to_owned(),
        is_dir,
        is_mention,
    }
}

#[test]
fn lists_files_after_the_text() {
    let message = message_with_files(
        "Resuma estes arquivos\n",
        &[
            listed("/tmp/a/page.html", false, true),
            listed("/tmp/a/my notes.md", false, true),
            listed("/tmp/a/src/", true, true),
            listed("/tmp/a/report.docx", false, false),
            listed("/tmp/a/paper.pdf", false, false),
        ],
    );
    assert_eq!(
        message,
        "Resuma estes arquivos\n\n<attached_files>\n@/tmp/a/page.html\n@\"/tmp/a/my notes.md\"\n\
         @/tmp/a/src/\n/tmp/a/report.docx\n/tmp/a/paper.pdf\n</attached_files>"
    );
    assert_eq!(message_with_files("oi", &[]), "oi");
    assert_eq!(
        message_with_files("", &[listed("/x.txt", false, true)]),
        "<attached_files>\n@/x.txt\n</attached_files>"
    );
    // A quote can't be inside a mention.
    assert_eq!(
        message_with_files("", &[listed("/a \"b\".txt", false, true)]),
        "<attached_files>\n/a \"b\".txt\n</attached_files>"
    );
}

#[test]
fn splits_listed_files_back_out() {
    let message = message_with_files(
        "Resuma",
        &[
            listed("/tmp/a/my notes.md", false, true),
            listed("/tmp/a/src", true, true),
            listed("/tmp/a/report.docx", false, false),
        ],
    );
    let (text, files) = split_attached_files(&message);
    assert_eq!(text, "Resuma");
    assert_eq!(
        files,
        vec![
            AttachedFile {
                path: "/tmp/a/my notes.md".to_owned(),
                name: "my notes.md".to_owned(),
                kind: AttachmentKind::Text,
            },
            AttachedFile {
                path: "/tmp/a/src".to_owned(),
                name: "src".to_owned(),
                kind: AttachmentKind::Folder,
            },
            AttachedFile {
                path: "/tmp/a/report.docx".to_owned(),
                name: "report.docx".to_owned(),
                kind: AttachmentKind::Document,
            },
        ]
    );

    let (text, files) = split_attached_files("Só texto, com <attached_files> no meio.");
    assert_eq!(text, "Só texto, com <attached_files> no meio.");
    assert!(files.is_empty());

    let (text, files) = split_attached_files("<attached_files>\n@/x.txt\n</attached_files>\n");
    assert_eq!(text, "");
    assert_eq!(files.len(), 1);
}
