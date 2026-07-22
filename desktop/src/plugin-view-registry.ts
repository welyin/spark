import type { Component } from 'vue';

type PluginViewKey = `${string}:${string}`;

const pluginViewMap = new Map<PluginViewKey, Component>();

function buildKey(pluginDomain: string, pluginView: string): PluginViewKey {
  return `${pluginDomain}:${pluginView}`;
}

export function registerPluginView(pluginDomain: string, pluginView: string, component: Component): void {
  if (!pluginDomain || !pluginDomain.startsWith('plugin:')) {
    throw new Error(`Invalid plugin domain for view registration: ${pluginDomain}`);
  }
  if (!pluginView || pluginView.trim().length === 0) {
    throw new Error('Plugin view id is required');
  }

  pluginViewMap.set(buildKey(pluginDomain, pluginView), component);
}

export function getPluginView(pluginDomain: string, pluginView = 'default'): Component | null {
  return pluginViewMap.get(buildKey(pluginDomain, pluginView)) ?? null;
}
