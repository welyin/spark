# 插件市场端到端（file:// 链路）

> 本文档从 [testing.md](testing.md) 拆分出来，独立覆盖插件市场 e2e 测试。

```bash
# 1) 构建插件产物并打包真实产物
cd code/plugins
npm run build:example      # vite 多入口构建 → spark-example/dist/
npm run package:example    # 打包签名 → app/dist-market/plugins/spark-example/

# 2) 市场服务单测 + 真实产物 e2e（opt-in）
cd ../app/src-tauri
cargo test --lib
SPARK_MARKET_E2E_RELEASE_DIR=$PWD/../dist-market/plugins \
SPARK_MARKET_E2E_PUBLIC_KEY_PEM="$(cat ../dist-market/plugins/spark-example/update-manifest.pub.pem)" \
  cargo test --lib e2e_real_release_artifacts
```

e2e 覆盖：默认公钥拒装（反）→ env 公钥 reconcile 标装（正）→ file:// 复制安装 → .spkg 内逐文件 sha256/size 一致性 → 同版本 check 为 up-to-date。
