<script setup lang="ts">
export type JournalOutlineItem = {
  level: number
  label: string
  line: number
  headingIndex: number
  children: JournalOutlineItem[]
}

defineOptions({ name: 'JournalOutlineTree' })

defineProps<{
  items: JournalOutlineItem[]
}>()

const emit = defineEmits<{
  select: [headingIndex: number]
}>()
</script>

<template>
  <ol class="journal-outline-list">
    <li v-for="item in items" :key="`${item.line}-${item.label}`" :data-level="item.level">
      <button
        type="button"
        :data-heading-index="item.headingIndex"
        :aria-label="`跳转到${item.label}`"
        :title="item.label"
        @click="emit('select', item.headingIndex)"
      >
        <span class="journal-outline-node" aria-hidden="true"></span>
        <strong>{{ item.label }}</strong>
      </button>
      <JournalOutlineTree
        v-if="item.children.length"
        :items="item.children"
        @select="emit('select', $event)"
      />
    </li>
  </ol>
</template>
