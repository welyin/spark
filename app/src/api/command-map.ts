/**
 * channel → command 映射表（自 api/index.ts 拆出，纯结构移动）。
 *
 * 职责：Electron 的 `ipcRenderer.invoke(channel, ...args)` 是"位置参数"，
 * Tauri 的 `invoke(command, args)` 是"命名参数对象"。本模块集中维护两张表：
 * `COMMAND_MAP` 把 channel（kebab-case）映射为 command（snake_case），
 * `ARG_NAMES` 按位置给出参数名（camelCase —— Tauri 自动映射到 Rust 的
 * snake_case 形参）。通用 call() 见 api/index.ts。
 */

/** Electron 通道名 → Tauri command 名（snake_case）。 */
export const COMMAND_MAP: Record<string, string> = {
  // 身份
  'root-status': 'root_status',
  'root-init': 'root_init',
  'root-unlock': 'root_unlock',
  'root-lock': 'root_lock',
  'root-list-identities': 'root_list_identities',
  'root-set-active': 'root_set_active',
  'root-recover-mnemonic': 'root_recover_mnemonic',
  'root-recover-backup': 'root_recover_backup',
  'root-backup-payload': 'root_backup_payload',
  'root-reveal-mnemonic': 'root_reveal_mnemonic',
  'root-update-profile': 'root_update_profile',
  'root-sign': 'root_sign',
  'root-derive-domain': 'root_derive_domain',
  'root-mnemonic-check': 'root_mnemonic_check',
  // 文档（plugin.doc* 手写包装，不走通用表）
  // 组织
  'org-list-mine': 'org_list_mine',
  'org-create': 'org_create',
  'org-invite-create': 'org_create_invite',
  'org-invite-accept': 'org_accept_invite',
  'org-sync-overview': 'org_sync_overview',
  'org-delete': 'org_delete',
  'org-add-member': 'org_add_member',
  'org-remove-member': 'org_remove_member',
  'org-set-gateways': 'org_set_gateways',
  'org-set-public': 'org_set_public',
  'org-resolve-address': 'org_resolve_address',
  'org-search-known': 'org_search_known',
  // 数据治理
  'data-usage': 'data_usage',
  'data-cleanup-now': 'data_cleanup_now',
  'data-export': 'data_export',
  'data-purge-preview': 'data_purge_preview',
  'data-purge-execute': 'data_purge_execute',
  // 存证
  'evidence-head-hash': 'evidence_head_hash',
  'evidence-verify': 'evidence_verify',
  // P2P
  'p2p-start': 'p2p_start',
  'p2p-stop': 'p2p_stop',
  'p2p-info': 'p2p_status',
  'p2p-broadcast': 'p2p_broadcast',
  'p2p-clear-peer-records': 'p2p_clear_peer_records',
  'p2p-sync-peer-organizations': 'p2p_sync_peer_organizations',
  'p2p-list-peer-records': 'p2p_list_peer_records',
  'p2p-get-dht-mode': 'p2p_get_dht_mode',
  'p2p-set-dht-mode': 'p2p_set_dht_mode',
  'p2p-make-node-card': 'p2p_make_node_card',
  'p2p-import-node-card': 'p2p_import_node_card',
  // 插件运行时（tab 模式语义，见 api/index.ts 注记）
  'plugin-identity-sign': 'plugin_identity_sign',
  'plugin-identity-verify': 'plugin_identity_verify',
  'plugin-org-sync-now': 'plugin_org_sync_now',
  // 插件市场
  'plugin-market-list': 'plugin_market_list',
  'plugin-market-check-updates': 'plugin_market_check_updates',
  'plugin-market-install': 'plugin_market_install',
  'plugin-market-upgrade': 'plugin_market_upgrade',
  'plugin-market-set-enabled': 'plugin_market_set_enabled'
};

/**
 * 各通道的位置参数名（camelCase；Tauri 将其映射到 Rust snake_case 形参）。
 * 缺省为 []（无参通道）。undefined 值会被 JSON 序列化丢弃 → Rust 侧得 None。
 */
export const ARG_NAMES: Record<string, string[]> = {
  'root-init': ['password', 'nickname', 'avatar'],
  'root-unlock': ['password', 'rootId'],
  'root-set-active': ['rootId'],
  'root-recover-mnemonic': ['mnemonic', 'newPassword', 'nickname', 'avatar'],
  'root-recover-backup': ['payload', 'password'],
  'root-reveal-mnemonic': ['password'],
  'root-sign': ['payload'],
  'root-derive-domain': ['domain'],
  'root-mnemonic-check': ['input'],
  'org-create': ['input'],
  'org-invite-create': ['orgId'],
  'org-invite-accept': ['code'],
  'org-sync-overview': ['orgId'],
  'org-delete': ['orgId'],
  'org-add-member': ['orgId', 'input'],
  'org-remove-member': ['orgId', 'memberRootId'],
  'org-set-gateways': ['orgId', 'gateways'],
  'org-set-public': ['orgId', 'public', 'displayName'],
  'org-resolve-address': ['orgAddress'],
  'org-search-known': ['keyword'],
  'data-export': ['filePath'],
  'data-purge-preview': ['orgId', 'beforeTs'],
  'data-purge-execute': ['orgId', 'beforeTs', 'confirmExported'],
  'p2p-broadcast': ['topic', 'message'],
  'p2p-sync-peer-organizations': ['targetPeer'],
  'p2p-set-dht-mode': ['mode'],
  'p2p-make-node-card': ['orgId'],
  'p2p-import-node-card': ['card'],
  'plugin-identity-sign': ['payload', 'pluginDomain'],
  'plugin-identity-verify': ['payload', 'signature', 'publicKey'],
  'plugin-org-sync-now': ['orgId', 'pluginDomain'],
  'plugin-market-check-updates': ['pluginId'],
  'plugin-market-install': ['pluginId'],
  'plugin-market-upgrade': ['pluginId'],
  'plugin-market-set-enabled': ['pluginId', 'enabled']
};
