<script setup lang="ts">
import { Markdown } from '@tiptap/markdown'
import StarterKit from '@tiptap/starter-kit'
import { EditorContent, useEditor } from '@tiptap/vue-3'
import {
  Bold,
  Code2,
  Heading2,
  Italic,
  List,
  ListOrdered,
  Quote,
  Redo2,
  Undo2,
} from 'lucide-vue-next'
import { watch } from 'vue'

const props = defineProps<{
  modelValue: string
  disabled?: boolean
}>()

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

const editor = useEditor({
  extensions: [StarterKit, Markdown],
  content: props.modelValue,
  contentType: 'markdown',
  editable: !props.disabled,
  editorProps: {
    attributes: {
      class: 'journal-markdown-surface',
      role: 'textbox',
      'aria-label': '日志 Markdown 编辑区',
      'aria-multiline': 'true',
    },
  },
  onUpdate: ({ editor: value }) => {
    emit('update:modelValue', value.getMarkdown())
  },
})

watch(() => props.modelValue, (value) => {
  if (!editor.value || editor.value.getMarkdown() === value) return
  editor.value.commands.setContent(value, { emitUpdate: false, contentType: 'markdown' })
})

watch(() => props.disabled, (disabled) => {
  editor.value?.setEditable(!disabled)
})

function focusHeading(headingIndex: number): void {
  if (!editor.value || !Number.isInteger(headingIndex) || headingIndex < 0) return
  let currentIndex = 0
  let headingPosition: number | null = null

  editor.value.state.doc.descendants((node, position) => {
    if (headingPosition !== null || node.type.name !== 'heading' || node.attrs.level > 3) return false
    if (currentIndex === headingIndex) {
      headingPosition = position
      return false
    }
    currentIndex += 1
    return false
  })

  if (headingPosition === null) return
  editor.value.chain().focus(headingPosition + 1).scrollIntoView().run()
}

defineExpose({ focusHeading })
</script>

<template>
  <div class="journal-markdown-editor" :class="{ 'is-disabled': disabled }">
    <div class="journal-format-toolbar" role="toolbar" aria-label="日志格式">
      <button
        type="button"
        aria-label="撤销"
        title="撤销"
        :disabled="disabled || !editor?.can().chain().focus().undo().run()"
        @click="editor?.chain().focus().undo().run()"
      >
        <Undo2 :size="17" aria-hidden="true" />
      </button>
      <button
        type="button"
        aria-label="重做"
        title="重做"
        :disabled="disabled || !editor?.can().chain().focus().redo().run()"
        @click="editor?.chain().focus().redo().run()"
      >
        <Redo2 :size="17" aria-hidden="true" />
      </button>
      <span aria-hidden="true"></span>
      <button
        type="button"
        aria-label="二级标题"
        title="二级标题"
        :class="{ 'is-active': editor?.isActive('heading', { level: 2 }) }"
        :disabled="disabled"
        @click="editor?.chain().focus().toggleHeading({ level: 2 }).run()"
      >
        <Heading2 :size="17" aria-hidden="true" />
      </button>
      <button
        type="button"
        aria-label="粗体"
        title="粗体"
        :class="{ 'is-active': editor?.isActive('bold') }"
        :disabled="disabled"
        @click="editor?.chain().focus().toggleBold().run()"
      >
        <Bold :size="17" aria-hidden="true" />
      </button>
      <button
        type="button"
        aria-label="斜体"
        title="斜体"
        :class="{ 'is-active': editor?.isActive('italic') }"
        :disabled="disabled"
        @click="editor?.chain().focus().toggleItalic().run()"
      >
        <Italic :size="17" aria-hidden="true" />
      </button>
      <button
        type="button"
        aria-label="无序列表"
        title="无序列表"
        :class="{ 'is-active': editor?.isActive('bulletList') }"
        :disabled="disabled"
        @click="editor?.chain().focus().toggleBulletList().run()"
      >
        <List :size="17" aria-hidden="true" />
      </button>
      <button
        type="button"
        aria-label="有序列表"
        title="有序列表"
        :class="{ 'is-active': editor?.isActive('orderedList') }"
        :disabled="disabled"
        @click="editor?.chain().focus().toggleOrderedList().run()"
      >
        <ListOrdered :size="17" aria-hidden="true" />
      </button>
      <button
        type="button"
        aria-label="引用"
        title="引用"
        :class="{ 'is-active': editor?.isActive('blockquote') }"
        :disabled="disabled"
        @click="editor?.chain().focus().toggleBlockquote().run()"
      >
        <Quote :size="17" aria-hidden="true" />
      </button>
      <button
        type="button"
        aria-label="代码块"
        title="代码块"
        :class="{ 'is-active': editor?.isActive('codeBlock') }"
        :disabled="disabled"
        @click="editor?.chain().focus().toggleCodeBlock().run()"
      >
        <Code2 :size="17" aria-hidden="true" />
      </button>
    </div>

    <EditorContent class="journal-editor-content" :editor="editor" />
  </div>
</template>
