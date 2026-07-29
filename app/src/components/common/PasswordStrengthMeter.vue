<template>
  <div v-if="password" class="pwd-meter">
    <div class="pwd-meter-bar">
      <span
        v-for="i in 3"
        :key="i"
        class="pwd-meter-seg"
        :class="[i <= level ? `on-${levelName}` : '']"
      />
    </div>
    <span class="pwd-meter-label" :class="`is-${levelName}`">{{ levelText }}</span>
    <p class="pwd-meter-tip">{{ tip }}</p>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';

export default defineComponent({
  name: 'PasswordStrengthMeter',
  props: {
    password: {
      type: String,
      default: ''
    }
  },
  setup(props) {
    /** 纯前端规则：长度 / 字母数字混合 / 符号，逐项计分 */
    const score = computed(() => {
      const pwd = props.password;
      if (!pwd) {
        return 0;
      }
      let s = 0;
      if (pwd.length >= 8) s += 1;
      if (pwd.length >= 12) s += 1;
      if (/[a-zA-Z]/.test(pwd) && /\d/.test(pwd)) s += 1;
      if (/[^a-zA-Z0-9]/.test(pwd)) s += 1;
      return s;
    });

    /** 1=弱 2=中 3=强（未满 8 位始终为弱） */
    const level = computed(() => {
      if (props.password.length < 8 || score.value <= 1) {
        return 1;
      }
      return score.value >= 3 ? 3 : 2;
    });

    const levelName = computed(() => ['weak', 'medium', 'strong'][level.value - 1]);
    const levelText = computed(() => ['弱', '中', '强'][level.value - 1]);

    const tip = computed(() => {
      const pwd = props.password;
      if (pwd.length < 8) {
        return '密码至少 8 位';
      }
      if (!/[a-zA-Z]/.test(pwd) || !/\d/.test(pwd)) {
        return '建议字母与数字混合';
      }
      if (pwd.length < 12) {
        return '建议 12 位以上，可加入符号更安全';
      }
      if (!/[^a-zA-Z0-9]/.test(pwd)) {
        return '建议加入符号（如 !@#）更安全';
      }
      return '密码强度良好';
    });

    return {
      level,
      levelName,
      levelText,
      tip
    };
  }
});
</script>

<style scoped>
.pwd-meter {
  display: grid;
  grid-template-columns: 1fr auto;
  align-items: center;
  gap: 4px 8px;
  margin-top: 6px;
  width: 100%;
}

.pwd-meter-bar {
  display: flex;
  gap: 4px;
}

.pwd-meter-seg {
  flex: 1;
  height: 4px;
  border-radius: var(--spark-radius-s);
  background: var(--spark-border-light);
  transition: background 0.2s;
}

.pwd-meter-seg.on-weak {
  background: var(--spark-danger);
}

.pwd-meter-seg.on-medium {
  background: var(--spark-warning);
}

.pwd-meter-seg.on-strong {
  background: var(--spark-success);
}

.pwd-meter-label {
  font-size: 12px;
}

.pwd-meter-label.is-weak {
  color: var(--spark-danger);
}

.pwd-meter-label.is-medium {
  color: var(--spark-warning);
}

.pwd-meter-label.is-strong {
  color: var(--spark-success);
}

.pwd-meter-tip {
  grid-column: 1 / -1;
  margin: 0;
  font-size: 12px;
  color: var(--spark-text-3);
}
</style>
