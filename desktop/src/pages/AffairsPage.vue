<template>
  <section class="affairs-page">
    <header class="page-header">
      <div class="page-header-main">
        <p class="eyebrow">待办</p>
        <h1>事务</h1>
        <p class="lede">组织内的待办与通知将汇总在这里。</p>
      </div>
    </header>

    <el-card shadow="never" class="panel-card">
      <el-empty description="这里是事务页面。" />
    </el-card>
  </section>
</template>

<script lang="ts">
import { defineComponent } from 'vue';

export default defineComponent({
  name: 'AffairsPage'
});
</script>

<style scoped src="../styles/pages/affairs.css"></style>
