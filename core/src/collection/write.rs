//! 写路径：put/delete —— 索引 diff、meta（含墓碑）与链式存证，单个原子 batch 提交。

use super::*;

impl DocumentCollection {
    /// 写入/替换文档：维护索引 diff、生成 meta（`{vv, ts, nodeId}`）、按需追加存证，
    /// 全部经一个原子 batch 提交。append-only 集合拒绝覆盖已存在文档。
    ///
    /// 返回广播所需的 meta 与策略声明副本（p2p 推送由调用方执行，对齐 TS
    /// `put` 末尾的非阻塞 broadcast）。
    pub fn put<S: StorageBackend>(
        &self,
        storage: &mut S,
        id: &str,
        doc: &Value,
        node_id: &str,
        now_ms: i64,
    ) -> Result<LocalWrite> {
        let policy = self.resolve_policy(storage)?;
        let existing = self.get(storage, id)?;
        if policy.sync_strategy == SyncStrategy::AppendOnly && existing.is_some() {
            return Err(CollectionError::AppendOnlyOverwrite {
                collection: self.collection.clone(),
                id: id.to_string(),
            });
        }
        let old_index_map = self.build_index_map_ordered(existing.as_ref());
        let new_index_map = self.build_index_map_ordered(Some(doc));
        let mut ops = vec![BatchOperation::put(
            self.doc_key(id),
            serde_json::to_string(doc)?,
        )];

        // 删除旧索引中已变化的项
        for (field, old_value) in &old_index_map {
            if !new_index_map
                .iter()
                .any(|(f, v)| f == field && v == old_value)
            {
                ops.push(BatchOperation::delete(self.index_key(field, old_value, id)));
            }
        }
        // 新增索引项
        for (field, new_value) in &new_index_map {
            if !old_index_map
                .iter()
                .any(|(f, v)| f == field && v == new_value)
            {
                ops.push(BatchOperation::put(
                    self.index_key(field, new_value, id),
                    "",
                ));
            }
        }

        // 新版 meta 与 doc 同 batch 提交
        let meta =
            generate_updated_meta(storage, node_id, &self.domain, &self.collection, id, now_ms)?;
        ops.push(BatchOperation::put(
            meta_key(&self.domain, &self.collection, id),
            serde_json::to_string(&meta)?,
        ));

        if policy.enable_evidence {
            let meta_value = serde_json::to_value(&meta)?;
            let entry = build_next_evidence_entry(
                storage,
                NewEvidenceEntry::from_parts(
                    &self.domain,
                    &self.collection,
                    id,
                    EvidenceOp::Put,
                    Some(doc),
                    Some(&meta_value),
                    now_ms,
                    node_id,
                ),
            )?;
            ops.extend(evidence_batch_operations(&entry)?);
        }

        storage.batch(ops)?;
        Ok(LocalWrite {
            meta,
            schema: Self::policy_declaration(&policy),
        })
    }

    /// 删除文档并清理索引；写墓碑 meta（`{vv, ts, tombstone: true}`）与存证。
    /// append-only 集合禁止删除；文档不存在时为空操作（返回 `Ok(None)`）。
    pub fn delete<S: StorageBackend>(
        &self,
        storage: &mut S,
        id: &str,
        node_id: &str,
        now_ms: i64,
    ) -> Result<Option<LocalWrite>> {
        let policy = self.resolve_policy(storage)?;
        if policy.sync_strategy == SyncStrategy::AppendOnly {
            return Err(CollectionError::AppendOnlyDelete(self.collection.clone()));
        }
        let Some(existing) = self.get(storage, id)? else {
            return Ok(None);
        };
        let mut ops = vec![BatchOperation::delete(self.doc_key(id))];
        for (field, value) in self.build_index_map_ordered(Some(&existing)) {
            ops.push(BatchOperation::delete(self.index_key(&field, &value, id)));
        }

        // 墓碑 meta：注意不带 nodeId（对齐 TS `{vv, ts, tombstone: true}`）
        let meta =
            generate_updated_meta(storage, node_id, &self.domain, &self.collection, id, now_ms)?;
        let tombstone = DocMeta {
            vv: meta.vv.clone(),
            ts: meta.ts,
            node_id: None,
            tombstone: Some(true),
        };
        ops.push(BatchOperation::put(
            meta_key(&self.domain, &self.collection, id),
            serde_json::to_string(&tombstone)?,
        ));

        if policy.enable_evidence {
            let tombstone_value = serde_json::to_value(&tombstone)?;
            let entry = build_next_evidence_entry(
                storage,
                NewEvidenceEntry::from_parts(
                    &self.domain,
                    &self.collection,
                    id,
                    EvidenceOp::Delete,
                    None,
                    Some(&tombstone_value),
                    now_ms,
                    node_id,
                ),
            )?;
            ops.extend(evidence_batch_operations(&entry)?);
        }

        storage.batch(ops)?;
        Ok(Some(LocalWrite {
            meta,
            schema: Self::policy_declaration(&policy),
        }))
    }
}
