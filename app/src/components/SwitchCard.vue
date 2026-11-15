<script setup>
const props = defineProps({
  title: { type: String, required: true },
  desc: { type: String, default: '' },
  enabled: { type: Boolean, default: false },
  busy: { type: Boolean, default: false },
  disabled: { type: Boolean, default: false }
})
const emit = defineEmits(['toggle'])

function onClick () {
  if (props.busy || props.disabled) return
  emit('toggle', !props.enabled)
}
</script>

<template>
  <div class="card">
    <div class="row" style="justify-content: space-between">
      <h3 style="margin:0">{{ title }}</h3>
      <button
        class="switch"
        :class="{ on: enabled }"
        :disabled="busy || disabled"
        :title="enabled ? '点击关闭' : '点击开启'"
        @click="onClick"
      />
    </div>
    <p v-if="desc" style="margin-top:8px">{{ desc }}</p>
    <p v-if="busy" class="dim" style="margin-top:6px">处理中…</p>
  </div>
</template>
