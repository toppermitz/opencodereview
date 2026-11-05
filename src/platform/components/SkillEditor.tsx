import type { Editor } from "@tiptap/core";
import { EditorContent, useEditor, useEditorState } from "@tiptap/react";
import {
  Bold,
  Code,
  CodeXml,
  Italic,
  Link,
  List,
  ListChecks,
  ListOrdered,
  type LucideIcon,
  Quote,
  Redo2,
  Strikethrough,
  Undo2,
} from "lucide-react";
import { useEffect, useRef } from "react";
import { skillEditorExtensions } from "../lib/skillEditor";

export function SkillEditor({
  content,
  disabled,
  onChange,
}: {
  content: string;
  disabled: boolean;
  onChange: (content: string) => void;
}) {
  const original = useRef(content);
  const normalized = useRef<string | null>(null);
  const editor = useEditor({
    extensions: skillEditorExtensions,
    content,
    contentType: "markdown",
    immediatelyRender: false,
    editable: !disabled,
    editorProps: {
      attributes: {
        class: "markdown skill-editor-document",
        role: "textbox",
        "aria-label": "Skill visual editor",
        "aria-multiline": "true",
        spellcheck: "false",
      },
      handlePaste: (view, event) => {
        if (event.clipboardData?.getData("text/html")) return false;
        const text = event.clipboardData?.getData("text/plain");
        if (!text || view.state.selection.$from.parent.type.name === "codeBlock") return false;
        editor?.commands.insertContent(text, { contentType: "markdown" });
        return true;
      },
    },
    onCreate: ({ editor: instance }) => {
      normalized.current = instance.getMarkdown();
    },
    onUpdate: ({ editor: instance }) => {
      const markdown = instance.getMarkdown();
      onChange(markdown === normalized.current ? original.current : markdown);
    },
  });
  useEffect(() => {
    editor?.setEditable(!disabled, false);
  }, [disabled, editor]);
  return (
    <div className="skill-visual-editor flex min-h-0 flex-1 flex-col">
      {editor && <SkillToolbar editor={editor} disabled={disabled} />}
      <div className="skill-editor-scroll min-h-0 flex-1 overflow-auto overscroll-contain [scrollbar-gutter:stable] [&>div]:min-h-full">
        <EditorContent editor={editor} />
      </div>
    </div>
  );
}

function SkillToolbar({ editor, disabled }: { editor: Editor; disabled: boolean }) {
  const state = useEditorState({
    editor,
    selector: ({ editor: instance }) => ({
      block: [1, 2, 3, 4, 5, 6].find((level) => instance.isActive("heading", { level })) ?? 0,
      bold: instance.isActive("bold"),
      italic: instance.isActive("italic"),
      strike: instance.isActive("strike"),
      code: instance.isActive("code"),
      bulletList: instance.isActive("bulletList"),
      orderedList: instance.isActive("orderedList"),
      taskList: instance.isActive("taskList"),
      blockquote: instance.isActive("blockquote"),
      codeBlock: instance.isActive("codeBlock"),
      link: instance.isActive("link"),
      undo: instance.can().undo(),
      redo: instance.can().redo(),
    }),
  });
  function link() {
    const href = window.prompt(
      "Link URL (https://, http:// or mailto:). Leave empty to remove.",
      editor.getAttributes("link").href ?? "",
    );
    if (href === null) return;
    if (!href.trim()) {
      editor.chain().focus().extendMarkRange("link").unsetLink().run();
      return;
    }
    if (!/^(https?:\/\/|mailto:)\S+$/i.test(href.trim())) return;
    editor.chain().focus().extendMarkRange("link").setLink({ href: href.trim() }).run();
  }
  const groups: Array<
    Array<{ label: string; icon: LucideIcon; active?: boolean; unavailable?: boolean; run: () => void }>
  > = [
    [
      {
        label: "Bold",
        icon: Bold,
        active: state.bold,
        run: () => {
          editor.chain().focus().toggleBold().run();
        },
      },
      {
        label: "Italic",
        icon: Italic,
        active: state.italic,
        run: () => {
          editor.chain().focus().toggleItalic().run();
        },
      },
      {
        label: "Strikethrough",
        icon: Strikethrough,
        active: state.strike,
        run: () => {
          editor.chain().focus().toggleStrike().run();
        },
      },
      {
        label: "Inline code",
        icon: Code,
        active: state.code,
        run: () => {
          editor.chain().focus().toggleCode().run();
        },
      },
      { label: "Link", icon: Link, active: state.link, run: link },
    ],
    [
      {
        label: "Bullet list",
        icon: List,
        active: state.bulletList,
        run: () => {
          editor.chain().focus().toggleBulletList().run();
        },
      },
      {
        label: "Numbered list",
        icon: ListOrdered,
        active: state.orderedList,
        run: () => {
          editor.chain().focus().toggleOrderedList().run();
        },
      },
      {
        label: "Task list",
        icon: ListChecks,
        active: state.taskList,
        run: () => {
          editor.chain().focus().toggleTaskList().run();
        },
      },
      {
        label: "Quote",
        icon: Quote,
        active: state.blockquote,
        run: () => {
          editor.chain().focus().toggleBlockquote().run();
        },
      },
      {
        label: "Code block",
        icon: CodeXml,
        active: state.codeBlock,
        run: () => {
          editor.chain().focus().toggleCodeBlock().run();
        },
      },
    ],
    [
      {
        label: "Undo",
        icon: Undo2,
        unavailable: !state.undo,
        run: () => {
          editor.chain().focus().undo().run();
        },
      },
      {
        label: "Redo",
        icon: Redo2,
        unavailable: !state.redo,
        run: () => {
          editor.chain().focus().redo().run();
        },
      },
    ],
  ];
  return (
    // biome-ignore lint/a11y/useSemanticElements: toolbar group; a <fieldset> adds UA min-inline-size that breaks the wrapping flex layout
    <div
      className="skill-editor-toolbar flex flex-none flex-wrap items-center gap-2 border-b border-line bg-surface px-6 py-2 [&_select]:h-8 [&_select]:w-30 [&_select]:rounded-md [&_select]:border [&_select]:border-line [&_select]:bg-surface [&_select]:px-2 [&_select]:text-caption [&_select]:text-fg-muted max-lg:px-4"
      role="group"
      aria-label="Text formatting"
    >
      <select
        aria-label="Text style"
        value={state.block}
        disabled={disabled}
        onChange={(event) => {
          const level = Number(event.target.value) as 0 | 1 | 2 | 3 | 4 | 5 | 6;
          if (level === 0) editor.chain().focus().setParagraph().run();
          else editor.chain().focus().setHeading({ level }).run();
        }}
      >
        <option value={0}>Parágrafo</option>
        {[1, 2, 3, 4, 5, 6].map((level) => (
          <option key={level} value={level}>
            Heading {level}
          </option>
        ))}
      </select>
      {groups.map((group) => (
        <div
          className="skill-toolbar-group flex gap-0.5 border-l border-line pl-2 [&_button]:grid [&_button]:size-8 [&_button]:cursor-pointer [&_button]:place-items-center [&_button]:rounded-md [&_button]:text-fg-muted [&_button:hover]:bg-surface-muted [&_button:hover]:text-fg [&_button[aria-pressed=true]]:bg-brand-soft [&_button[aria-pressed=true]]:text-brand"
          key={group.map((item) => item.label).join("|")}
        >
          {group.map(({ label, icon: Icon, active, unavailable, run }) => (
            <button
              type="button"
              key={label}
              title={label}
              aria-label={label}
              aria-pressed={active}
              disabled={disabled || unavailable}
              onMouseDown={(event) => event.preventDefault()}
              onClick={run}
            >
              <Icon className="size-4" />
            </button>
          ))}
        </div>
      ))}
    </div>
  );
}
