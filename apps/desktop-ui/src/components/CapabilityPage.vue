<script setup lang="ts">
import type { Component } from 'vue'

import CapabilityNotice from './CapabilityNotice.vue'

withDefaults(defineProps<{
  title: string
  description: string
  icon: Component
  reason?: string
}>(), {
  reason: 'not_implemented',
})
</script>

<template>
  <section class="page-section capability-page" :aria-labelledby="`${title}-heading`">
    <header class="page-heading">
      <div class="heading-icon" aria-hidden="true"><component :is="icon" :size="20" /></div>
      <div>
        <p class="eyebrow">本机管理</p>
        <h1 :id="`${title}-heading`">{{ title }}</h1>
        <p class="page-description">{{ description }}</p>
      </div>
    </header>
    <CapabilityNotice :reason="reason" />
  </section>
</template>
