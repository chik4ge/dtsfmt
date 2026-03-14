use std::collections::VecDeque;

use tree_sitter::{Node, TreeCursor};

use crate::config::Config;
use crate::context::Context;
use crate::layouts;
use crate::parser::parse;
use crate::utils::{
    get_text, lookahead, lookbehind, pad_right, print_indent, sep,
};

fn is_preproc(n: &tree_sitter::Node) -> bool {
    n.kind() == "preproc_include"
        || n.kind() == "preproc_ifdef"
        || n.kind() == "preproc_def"
        || n.kind() == "preproc_function_def"
}

struct DefineLine {
    name: String,
    value: String,
    comment: Option<String>,
}

fn node_text<'a>(source: &'a str, node: Node<'_>) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

fn split_define_value_comment(raw: &str) -> (String, Option<String>) {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    for (index, ch) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            '/' if !in_single_quote && !in_double_quote => {
                if raw[index..].starts_with("//") {
                    let value = raw[..index].trim().to_owned();
                    let comment = raw[index + 2..].trim();
                    let comment = match comment.is_empty() {
                        true => None,
                        false => Some(comment.to_owned()),
                    };

                    return (value, comment);
                }
            }
            _ => {}
        }
    }

    (raw.trim().to_owned(), None)
}

fn parse_define_line(source: &str, node: Node<'_>) -> DefineLine {
    let name = node
        .child_by_field_name("name")
        .map(|node| node_text(source, node).trim().to_owned())
        .unwrap_or_default();
    let raw_value =
        node.child_by_field_name("value").map(|node| node_text(source, node));
    let (value, comment) = raw_value
        .map(split_define_value_comment)
        .unwrap_or_else(|| (String::new(), None));

    DefineLine { name, value, comment }
}

fn is_define_block_start(cursor: &TreeCursor) -> bool {
    cursor.node().kind() == "preproc_def"
        && cursor
            .node()
            .prev_sibling()
            .is_none_or(|node| node.kind() != "preproc_def")
}

fn is_define_block_member(cursor: &TreeCursor) -> bool {
    cursor.node().kind() == "preproc_def"
        && cursor
            .node()
            .prev_sibling()
            .is_some_and(|node| node.kind() == "preproc_def")
}

fn print_define_block(
    writer: &mut String,
    source: &String,
    cursor: &mut TreeCursor,
    ctx: &Context,
) {
    let mut nodes = vec![cursor.node()];
    let mut last_node = cursor.node();

    while let Some(next_node) = last_node.next_sibling() {
        if next_node.kind() != "preproc_def" {
            break;
        }

        nodes.push(next_node);
        last_node = next_node;
    }

    let lines = nodes
        .into_iter()
        .map(|node| parse_define_line(source, node))
        .collect::<Vec<_>>();
    let name_width =
        lines.iter().map(|line| line.name.len()).max().unwrap_or(0);
    let value_width =
        lines.iter().map(|line| line.value.len()).max().unwrap_or(0);

    for line in lines {
        print_indent(writer, ctx);
        writer.push_str("#define ");
        writer.push_str(&line.name);

        if !line.value.is_empty() || line.comment.is_some() {
            let padding = name_width.saturating_sub(line.name.len()) + 1;
            writer.push_str(&" ".repeat(padding));
        }

        if !line.value.is_empty() {
            writer.push_str(&line.value);
        }

        if let Some(comment) = line.comment {
            let padding = value_width.saturating_sub(line.value.len()) + 2;
            writer.push_str(&" ".repeat(padding));
            writer.push_str("// ");
            writer.push_str(&comment);
        }

        writer.push('\n');
    }
}

fn traverse(
    writer: &mut String,
    source: &String,
    cursor: &mut TreeCursor,
    ctx: &Context,
) {
    let node = cursor.node();
    let kind = node.kind();

    match kind {
        "file_version" | "plugin" => {
            writer.push_str(&format!("{}\n\n", get_text(source, cursor)));
        }
        "comment" => {
            // Add a newline before the comment if the previous node is not a
            // comment nor a '{'.
            if lookbehind(cursor)
                .is_some_and(|n| n.kind() != "comment" && n.kind() != "{")
            {
                sep(writer);
            }

            print_indent(writer, ctx);
            let comment = get_text(source, cursor);

            // Only reformat single line comments, multi line comments are a
            // lot tougher to format properly.
            match comment.starts_with("//") {
                true => {
                    let trimmed_comment =
                        comment.trim_start_matches("//").trim();
                    if trimmed_comment.is_empty() {
                        writer.push_str("//");
                    } else {
                        writer.push_str("// ");
                        writer.push_str(trimmed_comment);
                    }
                }
                false => writer.push_str(comment),
            }

            writer.push('\n');
        }
        "dtsi_include" => {
            cursor.goto_first_child();
            print_indent(writer, ctx);
            writer.push_str("/include/ ");

            cursor.goto_next_sibling();
            writer.push_str(get_text(source, cursor));
            writer.push('\n');

            cursor.goto_parent();

            // Add a newline if this is the last dtsi_include
            if lookahead(cursor).is_some_and(|n| n.kind() != "dtsi_include") {
                writer.push('\n');
            }
        }
        "preproc_include" => {
            cursor.goto_first_child();
            print_indent(writer, ctx);
            writer.push_str("#include ");

            cursor.goto_next_sibling();
            writer.push_str(get_text(source, cursor));
            writer.push('\n');

            cursor.goto_parent();

            // Add a newline if this is the last preproc directive
            if lookahead(cursor).is_some_and(|n| !is_preproc(&n)) {
                writer.push('\n');
            }
        }
        "preproc_def" => {
            if ctx.config.align_define && is_define_block_start(cursor) {
                print_define_block(writer, source, cursor, ctx);
            } else if ctx.config.align_define && is_define_block_member(cursor) {
                if lookahead(cursor).is_some_and(|n| !is_preproc(&n)) {
                    writer.push('\n');
                }
                return;
            } else {
                print_indent(writer, ctx);
                writer.push_str("#define ");

                let node = cursor.node();
                let name = node
                    .child_by_field_name("name")
                    .map(|node| node_text(source, node).trim())
                    .unwrap_or("");
                writer.push_str(name);

                if let Some(value_node) = node.child_by_field_name("value") {
                    let value = node_text(source, value_node).trim();
                    if !value.is_empty() {
                        writer.push(' ');
                        writer.push_str(value);
                    }
                }

                writer.push('\n');
            }

            // Add a newline if this is the last preproc directive
            if lookahead(cursor).is_some_and(|n| !is_preproc(&n)) {
                writer.push('\n');
            }
        }
        "preproc_function_def" => {
            cursor.goto_first_child();
            writer.push_str("#define ");

            // Function and args
            for _ in 0..2 {
                cursor.goto_next_sibling();
                writer.push_str(get_text(source, cursor));
            }
            writer.push(' ');

            // Value
            cursor.goto_next_sibling();
            writer.push_str(get_text(source, cursor));

            writer.push('\n');
            cursor.goto_parent();

            // Add a newline if this is the last preproc directive
            if lookahead(cursor).is_some_and(|n| !is_preproc(&n)) {
                writer.push('\n');
            }
        }
        "preproc_ifdef" => {
            print_indent(writer, ctx);

            // #ifdef
            cursor.goto_first_child();
            writer.push_str(get_text(source, cursor).trim());
            writer.push(' ');

            // Name
            cursor.goto_next_sibling();
            writer.push_str(get_text(source, cursor));
            writer.push('\n');

            // Body
            while cursor.goto_next_sibling() {
                traverse(writer, source, cursor, ctx);
            }

            // Closing
            print_indent(writer, ctx);
            writer.push_str("#endif\n");
            cursor.goto_parent();

            // Add a newline if this is the last preproc directive
            if lookahead(cursor).is_some_and(|n| !is_preproc(&n)) {
                writer.push('\n');
            }
        }
        "identifier" | "string_literal" | "unit_address" | "path" => {
            writer.push_str(get_text(source, cursor));
        }
        "reference" => {
            // A reference has a format of "&label" or "&{path}".

            // Visit all children of the reference without changing the
            // indentation.
            if cursor.goto_first_child() {
                traverse(writer, source, cursor, ctx);
                while cursor.goto_next_sibling() {
                    traverse(writer, source, cursor, ctx);
                }
                cursor.goto_parent();
            }
        }
        // This is a general handler for any type that just needs to traverse
        // its children.
        "node" | "property" | "delete_node" | "delete_property" => {
            // A node will typically have children in a format of:
            // [<identifier>:] [&]<identifier> { [nodes and properties] }
            cursor.goto_first_child();

            // Nodes are preceded by a label or name identifier that need to be
            // indented. We can check for this by seeing if any siblings are
            // before us.
            if cursor.node().prev_sibling().is_none() {
                print_indent(writer, ctx);
            }

            // Increment the indentation for children and also check whether
            // we've identified a node keymap node for Zephyr-specific keymaps.
            let ctx = ctx.inc(1);
            let ctx = match get_text(source, cursor) {
                "keymap" => ctx.keymap(),
                "bindings" => ctx.bindings(),
                _ => ctx,
            };

            loop {
                traverse(writer, source, cursor, &ctx);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }

            // Return to the "node"'s node element to continue traversal.
            cursor.goto_parent();

            // Place a newline before node siblings if they follow a property.
            if node.kind() == "property"
                && lookahead(cursor).is_some_and(|n| n.kind() == "node")
            {
                writer.push('\n');
            }

            // Place a newline before node siblings if they follow a node.
            //
            // The `n != node` check is to prevent adding a newline before the
            // last node.
            if node.kind() == "node"
                && lookahead(cursor)
                    .is_some_and(|n| n.kind() == "node" && n != node)
            {
                writer.push('\n');
            }
        }
        "byte_string_literal" => {
            let hex_string = get_text(source, cursor);
            // Trim the [ and ] off of the source string we obtained.
            let hex_bytes = hex_string[1..hex_string.len() - 1]
                .split_whitespace()
                .collect::<Vec<&str>>();
            let hex_chunks = hex_bytes.chunks(16).collect::<Vec<&[&str]>>();

            // For smaller byte chunks it reads better if we just one line
            // everything, but for anything beyond 16 bytes we split it into
            // multiple lines.
            if hex_chunks.len() == 1 {
                writer.push_str(&format!("[{}]", hex_chunks[0].join(" ")));
            } else {
                writer.push_str("[\n");
                for (i, &line) in hex_chunks.iter().enumerate() {
                    print_indent(writer, ctx);
                    writer.push_str(&format!("{}\n", &line.join(" ")));
                    if i == hex_chunks.len() - 1 {
                        print_indent(writer, &ctx.dec(1));
                        writer.push(']');
                    }
                }
            }
        }

        "integer_cells" => {
            cursor.goto_first_child();

            // Keymap bindings are a special snowflake
            if ctx.has_zephyr_syntax() {
                print_bindings(writer, source, cursor, ctx);
                return;
            }

            writer.push('<');
            let mut first = true;

            while cursor.goto_next_sibling() {
                match cursor.node().kind() {
                    ">" => break,
                    _ => {
                        if first {
                            first = false;
                        } else {
                            writer.push(' ');
                        }

                        writer.push_str(get_text(source, cursor));
                    }
                }
            }

            writer.push('>');
            cursor.goto_parent();
        }
        // All the non-named grammatical tokens that are emitted but handled
        // simply with some output structure.
        "@" => {
            writer.push('@');
        }
        "&" => {
            writer.push('&');
        }
        "&{" => {
            writer.push_str("&{");
        }
        "}" => {
            print_indent(writer, &ctx.dec(1));
            writer.push('}');
        }
        "{" => {
            writer.push_str(" {\n");
        }
        ":" => {
            writer.push_str(": ");
        }
        ";" => {
            writer.push_str(";\n");
        }
        "," => {
            writer.push_str(", ");
        }
        "=" => {
            writer.push_str(" = ");
        }
        "/delete-node/" | "/delete-property/" => {
            writer.push_str(&format!("{} ", kind));
        }
        _ => {
            if ctx.config.warn_on_unhandled_tokens {
                eprintln!(
                    "unhandled type '{}' ({} {}): {}",
                    node.kind(),
                    node.child_count(),
                    if node.child_count() == 1 { "child" } else { "children" },
                    get_text(source, cursor)
                );
            }
            // Since we're unsure of this node just traverse its children
            if cursor.goto_first_child() {
                traverse(writer, source, cursor, ctx);

                while cursor.goto_next_sibling() {
                    traverse(writer, source, cursor, &ctx.inc(1));
                }

                cursor.goto_parent();
            }
        }
    };
}

fn collect_bindings(
    cursor: &mut TreeCursor,
    source: &String,
    ctx: &Context,
) -> VecDeque<String> {
    let mut buf: VecDeque<String> = VecDeque::new();
    let mut item = String::new();

    while cursor.goto_next_sibling() {
        match cursor.node().kind() {
            ">" => break,
            _ => {
                let text = get_text(source, cursor).trim();

                // If this is a new binding, add a new item to the buffer
                if !item.is_empty() && text.starts_with("&") {
                    buf.push_back(item);
                    item = String::new();
                }

                // Add a space between each piece of text
                if !item.is_empty() {
                    item.push(' ');
                }

                // Add the current piece of text to the buffer
                item.push_str(text);
            }
        }
    }

    // Add the last item to the buffer
    buf.push_back(item);

    // Move the items from the temporary buffer into a new vector that contains
    // the empty key spaces.
    layouts::get_layout(&ctx.config.layout)
        .bindings
        .iter()
        .map(|is_key| match is_key {
            1 => buf.pop_front().unwrap_or_default(),
            _ => String::new(),
        })
        .collect()
}

/// Calculate the maximum size of each column in the bindings table.
fn calculate_sizes(buf: &VecDeque<String>, row_size: usize) -> Vec<usize> {
    let mut sizes = Vec::new();

    for i in 0..row_size {
        let mut max = 0;

        for j in (i..buf.len()).step_by(row_size) {
            let len = buf[j].len();

            if len > max {
                max = len;
            }
        }

        sizes.push(max);
    }

    sizes
}

fn print_bindings(
    writer: &mut String,
    source: &String,
    cursor: &mut TreeCursor,
    ctx: &Context,
) {
    cursor.goto_first_child();
    writer.push('<');

    let buf = collect_bindings(cursor, source, ctx);
    let row_size = layouts::get_layout(&ctx.config.layout).row_size();
    let sizes = calculate_sizes(&buf, row_size);

    buf.iter().enumerate().for_each(|(i, item)| {
        let col = i % row_size;

        // Add a newline at the start of each row
        if col == 0 {
            writer.push('\n');
            print_indent(writer, ctx);
        }

        // Don't add padding to the last binding in the row
        let padding = match (i + 1) % row_size == 0 {
            true => 0,
            false => sizes[col] + 3,
        };

        writer.push_str(&pad_right(item, padding));
    });

    // Close the bindings
    writer.push('\n');
    print_indent(writer, &ctx.dec(1));
    writer.push('>');

    cursor.goto_parent();
}

pub fn print(source: &String, config: &Config) -> String {
    let mut writer = String::new();
    let tree = parse(source.clone());
    let mut cursor = tree.walk();

    let ctx =
        Context { indent: 0, bindings: false, keymap: false, config: config };

    // The first node is the root document node, so we have to traverse all it's
    // children with the same indentation level.
    cursor.goto_first_child();
    traverse(&mut writer, source, &mut cursor, &ctx);

    while cursor.goto_next_sibling() {
        traverse(&mut writer, source, &mut cursor, &ctx);
    }

    writer
}

#[cfg(test)]
mod tests {
    use super::print;
    use crate::config::Config;
    use crate::layouts::KeyboardLayoutType;

    fn config(align_define: bool) -> Config {
        Config::builder()
            .layout(KeyboardLayoutType::Adv360)
            .align_define(align_define)
            .build()
    }

    #[test]
    fn define_alignment_is_disabled_by_default() {
        let source = [
            "#define JP_DQUOTE AT // \"",
            "#define JP_UNDERSCORE LS(0x87) // _",
            "",
        ]
        .join("\n");

        let output = print(&source, &config(false));

        assert_eq!(
            output,
            [
                "#define JP_DQUOTE AT // \"",
                "#define JP_UNDERSCORE LS(0x87) // _",
                "",
            ]
            .join("\n")
        );
    }

    #[test]
    fn define_alignment_can_be_enabled() {
        let source = [
            "#define JP_DQUOTE AT // \"",
            "#define JP_UNDERSCORE LS(0x87) // _",
            "#define JP_KANA LANGUAGE_1 // kana",
            "#define JP_EMPTY",
            "#define JP_TRAILING VALUE",
            "",
        ]
        .join("\n");

        let output = print(&source, &config(true));

        assert_eq!(
            output,
            [
                "#define JP_DQUOTE     AT          // \"",
                "#define JP_UNDERSCORE LS(0x87)    // _",
                "#define JP_KANA       LANGUAGE_1  // kana",
                "#define JP_EMPTY",
                "#define JP_TRAILING   VALUE",
                "",
            ]
            .join("\n")
        );
    }

    #[test]
    fn define_alignment_does_not_stop_following_output() {
        let source = [
            "#define FIRST 1",
            "",
            "// comment",
            "#define SECOND 2 // two",
            "#define THIRD 3 // three",
            "",
            "&mt {",
            "  quick-tap-ms = <0>;",
            "};",
            "",
        ]
        .join("\n");

        let output = print(&source, &config(true));

        assert_eq!(
            output,
            [
                "#define FIRST 1",
                "",
                "// comment",
                "#define SECOND 2  // two",
                "#define THIRD  3  // three",
                "",
                "&mt {",
                "  quick-tap-ms = <0>;",
                "};",
                "",
            ]
            .join("\n")
        );
    }
}
