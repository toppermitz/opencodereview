import { readFileSync } from "node:fs";
import { Editor, type JSONContent } from "@tiptap/core";
import { afterEach, describe, expect, it } from "vitest";
import { skillEditorExtensions } from "./skillEditor";

let editor: Editor | undefined;
afterEach(() => editor?.destroy());

function open(content: string) {
  editor = new Editor({ extensions: skillEditorExtensions, content, contentType: "markdown" });
  return editor;
}

function semanticDocument(node: JSONContent): JSONContent {
  if (node.type === "codeBlock") return node;
  return {
    ...node,
    ...(node.text ? { text: node.text.replace(/\n[ \t]+/g, "\n") } : {}),
    ...(node.content ? { content: node.content.map(semanticDocument) } : {}),
  };
}

describe("skill Markdown editing", () => {
  it("round-trips the built-in skill without losing review templates or code fences", () => {
    const content = readFileSync("backend/src/assets/review-skill.md", "utf8");
    const instance = open(content);
    const before = instance.getJSON();
    instance.commands.setContent(instance.getMarkdown(), { contentType: "markdown" });
    expect(semanticDocument(instance.getJSON())).toEqual(semanticDocument(before));
    expect(instance.getText()).toContain("[Achado]");
    expect(instance.getText()).toContain("[Pergunta]");
  });

  it("preserves headings, marks, links, lists, tasks, tables and fenced code", () => {
    const instance = open(
      "# Skill\n\n**Evidence** and *reasoning* with ~~noise~~ and `code`.\n\n[Docs](https://example.com/docs)\n\n1. First\n2. Second\n\n- Item\n  - Nested\n\n- [x] Done\n- [ ] Pending\n\n| Rule | Result |\n| --- | --- |\n| Evidence | Required |\n\n> Quote\n\n```ts\nconst ok = true;\n```",
    );
    const before = instance.getJSON();
    const markdown = instance.getMarkdown();
    expect(markdown).toContain("- [x] Done");
    expect(markdown).toContain("| Evidence | Required |");
    expect(markdown).toContain("```ts");
    instance.commands.setContent(markdown, { contentType: "markdown" });
    expect(instance.getJSON()).toEqual(before);
  });

  it("preserves raw HTML and images as inert Markdown rather than executing or dropping them", () => {
    const instance = open(
      "Text <input> and <span>literal</span>.\n\n<!-- keep marker -->\n\n![Diagram](https://example.com/diagram.svg)",
    );
    const markdown = instance.getMarkdown();
    expect(markdown).toContain("<input>");
    expect(markdown).toContain("<span>literal</span>");
    expect(markdown).toContain("<!-- keep marker -->");
    expect(markdown).toContain("![Diagram](https://example.com/diagram.svg)");
    expect(instance.getHTML()).not.toContain("<input>");
    expect(instance.getHTML()).not.toContain("<img");
  });

  it("serializes visual edits as Markdown, not HTML", () => {
    const instance = open("# Skill\n\nEvidence");
    instance.commands.selectAll();
    instance.commands.toggleBold();
    expect(instance.getMarkdown()).toContain("**Evidence**");
    expect(instance.getMarkdown()).not.toContain("<strong>");
  });
});
