import { createApp } from 'vue';
import ElementPlus from 'element-plus';
import 'element-plus/dist/index.css';
import './styles/tokens.css';
import './styles/base.css';
import './styles/element-theme.css';
import './styles/app-shell.css';
import { initializePlugins } from './plugin-loader';
import { installHostApi } from './api';
import RootGate from './RootGate.vue';

// Tauri 环境：先把 window.electronAPI 安装为 invoke 实现，再启动应用
installHostApi();
initializePlugins();

createApp(RootGate).use(ElementPlus).mount('#app');
