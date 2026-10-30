use super::*;

fn image(data: &str) -> OutgoingImage {
    OutgoingImage {
        media_type: "image/png".to_owned(),
        data_base64: data.to_owned(),
    }
}

fn id(kind: TokenKind, number: usize) -> TokenId {
    TokenId { kind, number }
}

fn texts(message: &ComposedMessage) -> Vec<String> {
    message
        .blocks
        .iter()
        .map(|block| match block {
            OutgoingBlock::Text(text) => format!("text:{text}"),
            OutgoingBlock::Image(image) => format!("image:{}", image.data_base64),
            OutgoingBlock::Document(document) => format!("document:{}", document.data_base64),
        })
        .collect()
}

#[test]
fn formats_and_finds_tokens() {
    assert_eq!(token_text(id(TokenKind::Image, 1), ""), "[Imagem 1]");
    assert_eq!(token_text(id(TokenKind::Mention, 3), ""), "[Menção 3]");
    assert_eq!(
        token_text(id(TokenKind::File, 2), "notas [v2].pdf"),
        "[Arquivo 2: notas (v2).pdf]"
    );
    assert_eq!(
        token_text(id(TokenKind::Pasted, 1), &pasted_detail("a\nb\nc")),
        "[Texto colado 1 · 3 linhas]"
    );

    let text = "Veja [Imagem 1] e [Arquivo 2: a.pdf], [Menção 1] [Texto colado 4 · 20 linhas] \
                [Imagem] [Imagem x] [Arquivo 3] [Imagem 12345]";
    let found: Vec<(TokenId, &str)> = find_tokens(text)
        .into_iter()
        .map(|token| (token.id, &text[token.range]))
        .collect();
    assert_eq!(
        found,
        vec![
            (id(TokenKind::Image, 1), "[Imagem 1]"),
            (id(TokenKind::File, 2), "[Arquivo 2: a.pdf]"),
            (id(TokenKind::Mention, 1), "[Menção 1]"),
            (id(TokenKind::Pasted, 4), "[Texto colado 4 · 20 linhas]"),
        ]
    );
}

#[test]
fn detects_large_pastes() {
    assert!(!is_large_paste("curto"));
    assert!(!is_large_paste(&"linha\n".repeat(LARGE_PASTE_LINES)));
    assert!(is_large_paste(&"linha\n".repeat(LARGE_PASTE_LINES + 1)));
    assert!(!is_large_paste(&"a".repeat(LARGE_PASTE_CHARS)));
    assert!(is_large_paste(&"a".repeat(LARGE_PASTE_CHARS + 1)));
    assert_eq!(line_count("a\nb\n"), 2);
    assert_eq!(line_count(""), 1);
}

#[test]
fn composes_blocks_in_token_order() {
    let red = image("RED");
    let blue = image("BLUE");
    let pdf = OutgoingDocument {
        media_type: "application/pdf".to_owned(),
        data_base64: "PDF".to_owned(),
        title: Some("paper.pdf".to_owned()),
    };
    let paper = ListedFile {
        path: "/tmp/a/paper.pdf".to_owned(),
        is_dir: false,
        is_mention: false,
    };
    let code = ListedFile {
        path: "/tmp/a/main.rs".to_owned(),
        is_dir: false,
        is_mention: true,
    };
    // Image 1 was removed before sending, so its token is dropped and the others renumbered.
    let attachments = [
        DraftAttachment {
            id: id(TokenKind::Image, 3),
            content: DraftContent::Image(&blue),
        },
        DraftAttachment {
            id: id(TokenKind::Image, 2),
            content: DraftContent::Image(&red),
        },
        DraftAttachment {
            id: id(TokenKind::File, 1),
            content: DraftContent::File {
                file: &paper,
                document: Some(&pdf),
            },
        },
        DraftAttachment {
            id: id(TokenKind::File, 2),
            content: DraftContent::File {
                file: &code,
                document: None,
            },
        },
    ];
    let message = compose(
        "[Imagem 1] Compare [Imagem 2] com [Imagem 3], segundo [Arquivo 1: paper.pdf] e \
         [Arquivo 2: main.rs].",
        &attachments,
    );
    assert_eq!(
        texts(&message),
        vec![
            "text:Compare [Imagem 1]",
            "image:RED",
            "text: com [Imagem 2]",
            "image:BLUE",
            "text:, segundo [Arquivo 1: /tmp/a/paper.pdf]",
            "document:PDF",
            "text: e [Arquivo 2: @/tmp/a/main.rs].",
        ]
    );
    assert_eq!(
        message.text,
        "Compare [Imagem 1] com [Imagem 2], segundo [Arquivo 1: /tmp/a/paper.pdf] e [Arquivo 2: \
         @/tmp/a/main.rs]."
    );
    assert_eq!(message.images, vec![red, blue]);
}

#[test]
fn composes_mentions_and_pastes_as_blocks_where_they_were_put() {
    let pasted = "fn main() {\n    println!(\"oi\");\n}\n";
    let attachments = [
        DraftAttachment {
            id: id(TokenKind::Mention, 1),
            content: DraftContent::Mention("linha um\n\nlinha dois"),
        },
        DraftAttachment {
            id: id(TokenKind::Pasted, 1),
            content: DraftContent::Pasted(pasted),
        },
    ];
    let message = compose(
        "Sobre isto [Menção 1] o que acha? E deste código: [Texto colado 1 · 3 linhas] obrigado",
        &attachments,
    );
    assert_eq!(
        message.text,
        "Sobre isto\n\n> linha um\n>\n> linha dois\n\no que acha? E deste código:\n\n\
         <pasted_text label=\"Texto colado 1\" lines=\"3\">\nfn main() {\n    println!(\"oi\");\n}\n\
         </pasted_text>\n\nobrigado"
    );
    assert_eq!(texts(&message), vec![format!("text:{}", message.text)]);
}

#[test]
fn appends_attachments_whose_token_was_deleted() {
    let red = image("RED");
    let attachments = [
        DraftAttachment {
            id: id(TokenKind::Image, 1),
            content: DraftContent::Image(&red),
        },
        DraftAttachment {
            id: id(TokenKind::Mention, 1),
            content: DraftContent::Mention("citação"),
        },
    ];
    let message = compose("só texto  ", &attachments);
    assert_eq!(
        texts(&message),
        vec![
            "text:só texto\n\n[Imagem 1]",
            "image:RED",
            "text:\n\n> citação"
        ]
    );

    let only_image = compose("", &attachments[..1]);
    assert_eq!(texts(&only_image), vec!["text:[Imagem 1]", "image:RED"]);
}

#[test]
fn sends_a_copied_token_once() {
    let red = image("RED");
    let attachments = [DraftAttachment {
        id: id(TokenKind::Image, 1),
        content: DraftContent::Image(&red),
    }];
    let message = compose("[Imagem 1] e de novo [Imagem 1]", &attachments);
    assert_eq!(
        texts(&message),
        vec!["text:[Imagem 1]", "image:RED", "text: e de novo"]
    );
}

#[test]
fn lays_out_a_sent_message_in_order() {
    let text = "Compare [Imagem 1] com [Imagem 2], segundo [Arquivo 1: /tmp/a/paper.pdf] e \
                [Arquivo 2: @\"/tmp/a b/main.rs\"].\n\n> citado\n\ndepois\n\n\
                <pasted_text label=\"Texto colado 1\" lines=\"2\">\nx\ny\n</pasted_text>\n\nfim \
                [Imagem 7] [Arquivo 3: nome.pdf]";
    let layout = message_layout(text, 3);
    let file = |path: &str, name: &str| {
        MessagePart::File(AttachedFile {
            path: path.to_owned(),
            name: name.to_owned(),
            kind: attachments::classify(path, false),
        })
    };
    assert_eq!(
        layout.parts,
        vec![
            MessagePart::Text("Compare".to_owned()),
            MessagePart::Image(0),
            MessagePart::Text("com".to_owned()),
            MessagePart::Image(1),
            MessagePart::Text(", segundo".to_owned()),
            file("/tmp/a/paper.pdf", "paper.pdf"),
            MessagePart::Text("e".to_owned()),
            file("/tmp/a b/main.rs", "main.rs"),
            MessagePart::Text(".".to_owned()),
            MessagePart::Quote("citado".to_owned()),
            MessagePart::Text("depois".to_owned()),
            MessagePart::Pasted {
                text: "x\ny".to_owned(),
                lines: 2,
            },
            MessagePart::Text("fim [Imagem 7] [Arquivo 3: nome.pdf]".to_owned()),
        ]
    );
    assert_eq!(layout.loose_images, vec![2]);
}

#[test]
fn lays_out_a_message_composed_from_blocks_joined_by_history() {
    let red = image("RED");
    let attachments = [
        DraftAttachment {
            id: id(TokenKind::Image, 1),
            content: DraftContent::Image(&red),
        },
        DraftAttachment {
            id: id(TokenKind::Mention, 1),
            content: DraftContent::Mention("trecho"),
        },
    ];
    let message = compose("antes [Imagem 1] meio [Menção 1] fim", &attachments);
    let joined = message
        .blocks
        .iter()
        .filter_map(|block| match block {
            OutgoingBlock::Text(text) => Some(text.as_str()),
            OutgoingBlock::Image(_) | OutgoingBlock::Document(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let expected = vec![
        MessagePart::Text("antes".to_owned()),
        MessagePart::Image(0),
        MessagePart::Text("meio".to_owned()),
        MessagePart::Quote("trecho".to_owned()),
        MessagePart::Text("fim".to_owned()),
    ];
    assert_eq!(message_layout(&joined, 1).parts, expected);
    assert_eq!(message_layout(&message.text, 1).parts, expected);
}

#[test]
fn lays_out_legacy_messages() {
    let layout = message_layout(
        "> Ajuste 2: \"Retorno\" de volta\n\n> linha um\n>\n> linha dois\n\npergunta",
        2,
    );
    assert_eq!(
        layout.parts,
        vec![
            MessagePart::Quote("Ajuste 2: \"Retorno\" de volta".to_owned()),
            MessagePart::Quote("linha um\n\nlinha dois".to_owned()),
            MessagePart::Text("pergunta".to_owned()),
        ]
    );
    assert_eq!(layout.loose_images, vec![0, 1]);
}

#[test]
fn shows_pastes_as_tokens_in_previews() {
    assert_eq!(
        display_text("<pasted_text label=\"Texto colado 1\" lines=\"40\">\n…\n</pasted_text>\noi"),
        "[Texto colado 1 · 40 linhas]\noi"
    );
}

#[test]
fn renumbers_tokens() {
    let text = "a [Imagem 1] b [Arquivo 1: x.rs] c [Imagem 2]";
    let renumbered = renumber_tokens(text, |id| match id.kind {
        TokenKind::Image => Some(id.number + 4),
        TokenKind::File | TokenKind::Mention | TokenKind::Pasted => None,
    });
    assert_eq!(renumbered, "a [Imagem 5] b [Arquivo 1: x.rs] c [Imagem 6]");
}

#[test]
fn finds_what_is_left_of_a_partly_deleted_token() {
    let old = "olha [Imagem 1] aqui";
    // Backspace on the closing bracket.
    assert_eq!(
        token_remnant(old, "olha [Imagem 1 aqui"),
        Some((id(TokenKind::Image, 1), 5..14))
    );
    // Delete on the opening bracket.
    assert_eq!(
        token_remnant(old, "olha Imagem 1] aqui"),
        Some((id(TokenKind::Image, 1), 5..14))
    );
    // A selection from the text into the token.
    assert_eq!(
        token_remnant(old, "olhagem 1] aqui"),
        Some((id(TokenKind::Image, 1), 4..10))
    );
    // The whole token, a character outside it, and typing.
    assert_eq!(token_remnant(old, "olha  aqui"), None);
    assert_eq!(token_remnant(old, "olh [Imagem 1] aqui"), None);
    assert_eq!(token_remnant(old, "olha [Imagem 1] aquix"), None);
    let accented = "é [Menção 2] ã";
    assert_eq!(
        token_remnant(accented, "é [Menção 2 ã"),
        Some((id(TokenKind::Mention, 2), 3..14))
    );
}

#[test]
fn writes_the_text_of_a_layout_with_tokens() {
    let layout = message_layout(
        "Compare [Imagem 1] com [Imagem 2], segundo [Arquivo 1: /tmp/a/paper.pdf].\n\n> citado\n\n\
         depois\n\n<pasted_text label=\"Texto colado 1\" lines=\"2\">\nx\ny\n</pasted_text>",
        2,
    );
    let (text, tokens) = text_with_tokens(&layout.parts);
    assert_eq!(
        text,
        "Compare [Imagem 1] com [Imagem 2], segundo [Arquivo 1: paper.pdf]. [Menção 1] depois \
         [Texto colado 1 · 2 linhas]"
    );
    let found: Vec<(String, TokenKind)> = tokens
        .into_iter()
        .map(|(range, kind)| {
            (
                text.chars().skip(range.start).take(range.len()).collect(),
                kind,
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            ("[Imagem 1]".to_owned(), TokenKind::Image),
            ("[Imagem 2]".to_owned(), TokenKind::Image),
            ("[Arquivo 1: paper.pdf]".to_owned(), TokenKind::File),
            ("[Menção 1]".to_owned(), TokenKind::Mention),
            ("[Texto colado 1 · 2 linhas]".to_owned(), TokenKind::Pasted),
        ]
    );
}

#[test]
fn keeps_labels_inside_quotes_as_text() {
    let layout = message_layout(
        "> antes eu mandei [Imagem 1] e [Imagem 2]\n\nagora [Imagem 1]",
        1,
    );
    assert_eq!(
        layout.parts,
        vec![
            MessagePart::Quote("antes eu mandei [Imagem 1] e [Imagem 2]".to_owned()),
            MessagePart::Text("agora".to_owned()),
            MessagePart::Image(0),
        ]
    );
}

#[test]
fn keeps_the_cursor_and_selections_out_of_tokens() {
    let text = "ab [Imagem 1] cd";
    let tokens = find_tokens(text);
    // A cursor moving right into the token jumps past it, moving left jumps before it.
    assert_eq!(snap_selection(&tokens, 4, 4, Some(3)), Some((13, 13)));
    assert_eq!(snap_selection(&tokens, 12, 12, Some(13)), Some((3, 3)));
    // A click goes to the nearer edge.
    assert_eq!(snap_selection(&tokens, 5, 5, None), Some((3, 3)));
    assert_eq!(snap_selection(&tokens, 11, 11, None), Some((13, 13)));
    assert_eq!(snap_selection(&tokens, 3, 3, None), None);
    // A selection grows to the whole token, keeping its direction.
    assert_eq!(snap_selection(&tokens, 0, 6, None), Some((0, 13)));
    assert_eq!(snap_selection(&tokens, 15, 8, None), Some((15, 3)));
    assert_eq!(snap_selection(&tokens, 0, 13, None), None);
}

#[test]
fn moves_text_typed_inside_a_token_after_it() {
    let old = "ab [Imagem 1] cd";
    let tokens = find_tokens(old);
    assert_eq!(
        move_insertion_out_of_token(old, "ab [Imagetestem 1] cd", &tokens),
        Some(("ab [Imagem 1]teste cd".to_owned(), 18))
    );
    assert_eq!(
        move_insertion_out_of_token(old, "ab [Imagemteste 1] cd", &tokens),
        Some(("ab [Imagem 1]teste cd".to_owned(), 18))
    );
    assert_eq!(
        move_insertion_out_of_token(old, "abx [Imagem 1] cd", &tokens),
        None
    );
    assert_eq!(
        move_insertion_out_of_token(old, "ab [Imagem 1]x cd", &tokens),
        None
    );
}

#[test]
fn editor_tokens_wrap_whole_and_still_parse() {
    let token = editor_token_text(id(TokenKind::Pasted, 2), "3 linhas");
    assert_eq!(
        token,
        "[Texto\u{a0}colado\u{a0}2\u{a0}·\u{a0}3\u{a0}linhas]"
    );
    assert!(!token.contains(' '));
    let file = editor_token_text(id(TokenKind::File, 1), "meu arquivo.pdf");
    let text = format!("a {token} b {file} c [Imagem 3]");
    let found: Vec<TokenId> = find_tokens(&text)
        .into_iter()
        .map(|token| token.id)
        .collect();
    assert_eq!(
        found,
        vec![
            id(TokenKind::Pasted, 2),
            id(TokenKind::File, 1),
            id(TokenKind::Image, 3)
        ]
    );
    // Renumbering only changes the numbers, keeping the no-break spaces.
    let renumbered = renumber_tokens(&text, |id| Some(id.number + 9));
    assert_eq!(
        renumbered,
        "a [Texto\u{a0}colado\u{a0}11\u{a0}·\u{a0}3\u{a0}linhas] b \
         [Arquivo\u{a0}10:\u{a0}meu\u{a0}arquivo.pdf] c [Imagem 12]"
    );
}

#[test]
fn composes_editor_tokens_with_plain_spaces() {
    let red = image("RED");
    let attachments = [DraftAttachment {
        id: id(TokenKind::Image, 1),
        content: DraftContent::Image(&red),
    }];
    let text = format!(
        "veja {} aqui",
        editor_token_text(id(TokenKind::Image, 1), "")
    );
    let message = compose(&text, &attachments);
    assert_eq!(message.text, "veja [Imagem 1] aqui");
}

fn link(key: u64, kind: TokenKind, number: usize) -> Link {
    Link {
        key,
        id: id(kind, number),
    }
}

#[test]
fn finds_attachments_whose_token_is_gone() {
    let links = [
        link(1, TokenKind::Image, 1),
        link(2, TokenKind::Image, 2),
        link(3, TokenKind::Pasted, 1),
    ];
    assert_eq!(
        unlinked("[Imagem 1] e [Texto colado 1 · 3 linhas]", &links),
        vec![2]
    );
    assert_eq!(unlinked("", &links), vec![1, 2, 3]);
    // A partial token doesn't count.
    assert_eq!(
        unlinked("[Imagem 1 [Imagem 2] [Texto colado 1]", &links),
        vec![1, 3]
    );
}

#[test]
fn compacts_numbers_per_kind_in_order() {
    let links = [
        link(7, TokenKind::Image, 3),
        link(8, TokenKind::Image, 1),
        link(9, TokenKind::File, 4),
    ];
    assert_eq!(
        compact_numbers(&links, &[TokenKind::Image]),
        vec![(8, 1), (7, 2)]
    );
    assert_eq!(
        compact_numbers(&links, &[TokenKind::Image, TokenKind::File]),
        vec![(8, 1), (7, 2), (9, 1)]
    );
}

#[test]
fn restores_the_links_of_a_text_undone_back_to() {
    let mut history = LinkHistory::default();
    let before = "a [Imagem 1] b [Imagem 2] c [Imagem 3]";
    history.note(
        before,
        vec![
            link(1, TokenKind::Image, 1),
            link(2, TokenKind::Image, 2),
            link(3, TokenKind::Image, 3),
        ],
    );
    // Image 2's token is deleted: the state before and the one right after are saved, then the
    // last image is renumbered.
    let deleted = "a [Imagem 1] b  c [Imagem 3]";
    history.save(
        deleted,
        vec![link(1, TokenKind::Image, 1), link(3, TokenKind::Image, 3)],
    );
    history.note(
        "a [Imagem 1] b  c [Imagem 2]",
        vec![link(1, TokenKind::Image, 1), link(3, TokenKind::Image, 2)],
    );

    // Undoing the renumbering gives image 3 its number back.
    assert_eq!(
        history.find(deleted, &[1, 3]),
        Some(&[link(1, TokenKind::Image, 1), link(3, TokenKind::Image, 3)][..])
    );
    // Undoing the deletion brings image 2 back.
    assert_eq!(history.find(before, &[1, 3]).map(<[Link]>::len), Some(3));
    assert!(history.remembers(2));
    // An attachment added since makes an equal text another draft.
    assert_eq!(history.find(before, &[1, 3, 4]), None);
    assert_eq!(history.find("outro", &[1]), None);

    history.clear();
    assert_eq!(history.find(before, &[1, 3]), None);
    assert!(!history.remembers(2));
}
