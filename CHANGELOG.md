# Changelog

このファイルの形式は [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) に従います。バージョニング方針は本文末尾の「セマンティックバージョニング」を参照してください。

## [Unreleased]

### Added

- **組み込みの MCP サーバー**（`rustxng mcp`、stdio）— 公式 Rust SDK `rmcp` で `search`（順位付きスニペット）と `read_page`（検索結果のページ本文を文字単位で分割して読む）を提供。HTTP API と同じ `Searcher` を共有し、任意 URL の取得はできない（#30）

## [0.26.0] - 2026-11-01

導入のしやすさと発見性を整え、OSS として配布物を完成させました。

### Added

- **Docker イメージを GHCR へ公開**（`.github/workflows/docker.yml`）— タグ push で `ghcr.io/sahenjp/rustxng:<version>` と `:latest` を発行。公開後にコンテナを起動して `/healthz` を確認するスモークテスト付き
- **英語 README を主に**（`README.md`）、日本語は `README.ja.md` へ。相互リンクを更新
- **導入方法を4通り明記**: ビルド済みバイナリ / `cargo install` / Docker / ソースから
- **リポジトリの説明とトピックを AI 向けに更新**（ai-agents / grounding / llm / mcp / rag / self-hosted を追加）
- **ラベル整備**（engine / ranking / performance / security / benchmark / backlog）

### Fixed

- **README の Docker 手順が事実と違っていた** — 未公開の `ghcr.io` イメージから取得するよう書いていた。公開ワークフローを追加し、記載を実態に合わせた
- **28 MB の JSON が git に追跡されていた** — `bench/content-*.json` は再生成可能なため追跡を外した（HEAD の最大ファイルは 896 KB に）。履歴の掃除手順は CONTRIBUTING に記載

### Tests

- 797 ユニット + 17 統合、clippy 0、markdownlint 0

## [0.25.0] - 2026-11-01

**AI エージェントの情報取得層**として位置づけを明確にし、その前提となる SSRF 対策を強化しました。

### Security

- **`include_raw_content` のページ取得に SSRF 対策** — 検索結果の URL は第三者のエンジンが返すため信頼できません。`http://169.254.169.254/`（クラウドメタデータ）や `http://127.0.0.1:8080/`（社内サービス）を指す結果でも、素朴な実装はそのまま取得してしまいます。`image_proxy` / `favicon_proxy` と同じ「公開アドレスのみ」判定を、本文取得の直前に適用しました

### Changed

- **README を AI インフラとして再構成** — 「SearXNG の Rust 版」ではなく「ローカル AI エージェントの検索・Grounding 層」として、エージェントに必要な性質（鍵不要・ローカル完結・本文取得・初回以外 0.01 秒・SSRF 対策）を先頭に提示
- **docs/AI-INFRA.md を追加** — 位置づけの根拠、SSRF が AI 用途で必須である理由、「テスト自動化とセキュリティ監査が不可欠な理由」（静かに壊れる4パターンと対策）

### なぜテストと監査が不可欠か（doc に明記）

| 壊れ方 | 気づきにくい理由 | 対策 |
| --- | --- | --- |
| パーサーが静かに 0 件を返す | エラーにならず「結果なし」に見える | 797 ユニット + 17 統合 |
| エンジンが一斉にレート制限 | 成功するが精度だけ落ちる | nightly ベンチマーク（下限割れで CI 失敗） |
| 依存の脆弱性 | 動作は変わらないまま穴が開く | `cargo audit`（既定ビルド 0 件） |
| SSRF の穴 | 正常な検索では表面化しない | 公開アドレス限定をテストで固定 |

### Tests

- 797 ユニット + 17 統合

## [0.24.0] - 2026-10-31

ブロックされたエンジンだけをプロキシ経由にできるようにしました（上流 SearXNG の per-engine `proxies` 相当）。

### Added

- **エンジン個別のプロキシ**（`[[engines]]` の `proxy`）
  - 指定したエンジンだけ専用クライアントでアクセスし、それ以外は直接アクセスを続ける
  - プロキシ URL ごとにクライアントをキャッシュするので、同じ設定のエンジンは接続を共有する
  - 実機検証: ローカルに試験プロキシを立て、`mwmbl` にだけ `proxy` を設定 → プロキシ側に `CONNECT api.mwmbl.org:443` が記録され、`swisscows` は記録されないことを確認

### ブロックの実態（実測）

新鮮なインスタンスで 1 回検索したときの内訳: 応答 44 / 無応答 25。

| 理由 | 件数 | プロキシで改善するか |
| --- | ---: | --- |
| Access denied (403) | 12 | IP 由来なら改善。UA 由来なら効果なし |
| CAPTCHA | 7 | IP の評判由来なら改善。エンジン側のbot判定なら効果なし |
| Too many requests | 1 | 分散すれば改善 |
| Timeout | 2 | 改善しない |
| HTTP error | 1 | 要調査 |

つまり**プロキシは万能ではありません**。403 と CAPTCHA の一部に効く可能性がある、というのが実測からの結論です（この環境にはプロキシがないため、実際の解除効果は未検証）。

### Tests

- 797 ユニット + 17 統合（エンジン個別プロキシのマージのテストを追加）

## [0.23.1] - 2026-10-31

インデックスを育てた結果の実測を記録しました（コード変更なし）。

### 実測

| インデックス | hit@1 | hit@5 | MRR |
| --- | ---: | ---: | ---: |
| なし | 79 | 92 | 0.853 |
| 5,000 ページ | **83** | **96** | **0.889** |
| 7,200 ページ（sitemap） | 81 | 93 | 0.871 |
| 15,000 ページ（広いシード） | 80 | 93 | 0.863 |

5,000 ページで頭打ち。ページを増やしても上流が既に取れているページを重ねるだけで、
精度は上がらない。自前インデックスの価値は精度ではなく速度（将来の即答）にある。

## [0.23.0] - 2026-10-31

自前インデックスを**実験機能（既定オフ）**に戻し、クローラーを実運用向けに直しました。

### Changed

- **`local_index_path` の既定を空に** — 自前インデックスは実験機能になりました。設定した人だけが使います（クロールとインデックス管理が必要なため）
- docs/LOCAL-INDEX.md の冒頭に実験機能である旨を明記

### Fixed

- **robots.txt の解析でパニック** — 12 バイト目で切っていたため、日本語コメントを含む robots.txt でマルチバイト境界を割って落ちていました。文字列前方一致に変更
- **再クロールで重複登録** — 既にインデックスにある URL をスキップする `has_url` を追加。中断したクロールを続きから再開できます
- **長時間クロールの進捗が見えない** — stdout を明示的に flush し、50 ページごとに進捗を出すようにしました（ファイルへリダイレクトすると行バッファされないため）

### 実測

- クロール速度: **約 575 ページ/分**（24 ワーカー、ホストごと 120 ms 間隔）
- 再開時の重複なし: 7,200 ページのインデックスに 1,150 ページ追加して 8,350 件

### Tests

- 796 ユニット + 17 統合

## [0.22.0] - 2026-10-31

計測の自動化・エンジンの自動管理・クロールの網羅性を入れました。

### Added

- **nightly ベンチマーク CI**（`.github/workflows/nightly-bench.yml`）— 毎日 100 問を回し、結果を artifact とジョブサマリに残す。`tools/check_bench.py` が下限（hit@1 50・失敗 0）を割ったら失敗させる。マシン差で数問は振れるため、回帰の検知ではなく**破損の検知**に絞った設計
- **エンジンの自動休止** — 結果を返さないまま成功し続けたエンジンを一定時間スキップする（`empty_result_limit` 既定 8 回 / `empty_result_cooldown` 既定 300 秒）。ブロックされて空の応答を返すエンジンが毎回タイムアウト分を消費するのを防ぐ。1 件でも返せば即復帰
- **sitemap 対応** — `rustxng-index crawl --sitemaps` で robots.txt の `Sitemap:` を辿る。sitemap index の再帰と、平文の `sitemap.txt` の両方に対応。リンク辿りより網羅的
- README にエンジン自動管理・プロキシ・サイドカー用途の節

### Changed

- 未検証エンジンを再調査し、実際に結果を返した `google_scholar` と `rottentomatoes` を有効化（`microsoft_learn` / `lib_rs` は既に有効）
- **プロキシは実装済みであることを確認**（`outgoing.proxies`、scheme ごとに指定）。未対応点はエンジン個別のプロキシ指定のみと明記

### Tests

- 796 ユニット + 17 統合（エンジン休止のテストを追加）

## [0.21.1] - 2026-10-31

rdsh が起動するサイドカーが残り続ける問題に対応しました。

### Added

- **アイドル終了** — `server.idle_timeout`（秒、既定 0 = 無効）または環境変数 `RUSTXNG_IDLE_TIMEOUT`。指定秒数リクエストが無ければ正常終了します。rdsh は `search-web` のたびに RustXNG をサイドカーとして起動するため、終了しないプロセスが溜まることがありました
- 実機で確認: `RUSTXNG_IDLE_TIMEOUT=8` で起動し、リクエスト後に 8 秒待つと `idle timeout reached, shutting down` を出して終了

### Docs

- docs/RDSH.md にサイドカーの後始末を追記

## [0.21.0] - 2026-10-31

整理・脆弱性対応・推奨設定の既定化・rdsh 連携を行いました。

### Changed

- **自前インデックスのモードを 1 つに削減** — 4 モード（off / fast / merge / both）を廃止し、`local_index_merge`（真偽値）だけにしました。実測で「ローカルだけで即答する」方式は精度を落とす（72/82）ため、上流との併合だけを残しています
- **推奨設定を既定に** — `local_index_path = "index-data"` / `local_index_merge = true`。`index-data/` を置くだけで併用が始まります（ディレクトリが無ければ無効）

### Security

- **依存の脆弱性 6 件 → 1 件**（`cargo audit`）
  - quick-xml 0.36 → **0.41**（DoS 2 件、CVSS 7.5。API 移行とエンティティ参照の解決を実装）
  - rustls-webpki 0.103.15 → 0.103.16、webpki-roots 1.0.9 → 1.0.11、libz-sys 1.1.30 → 1.1.31
  - 残る 1 件は ctutils（上流の digest が固定）で、**既定ビルドには含まれない** Postgres オプション機能のみ
- **SECURITY.md を追加** — スキャン結果、解消済み・未解消の内訳、実行時の防御を明記

### Added

- **docs/RDSH.md** — rdsh から使う手順。既定ポート 8888 が rdsh の探索先と一致するため追加設定は不要。`rdsh search-web --engine rustxng --json` が実際に解釈できることを確認
- README に自前インデックスと rdsh 連携を追記

### Removed

- 探索用の評価データ 40 ファイル（`bench/` を 58 → 18 ファイルへ）

### Tests

- 795 ユニット + 17 統合、clippy 0、markdownlint 0

## [0.20.2] - 2026-10-31

インデックス規模と精度の関係を記録しました（コード変更なし）。

### Added

- **規模と精度の実測**（docs/LOCAL-INDEX.md、bench/eval-100-local7600.json）

| インデックス | hit@1 | hit@5 | MRR |
| --- | ---: | ---: | ---: |
| なし | 79 | 92 | 0.853 |
| 5,000 ページ | **83** | **96** | **0.889** |
| 7,600 ページ | **83** | **96** | 0.888 |

**5,000 ページで頭打ち**。上流が既に取れているページを重複して足しても伸びないため、merge の効果は「上流が落としているページを拾えるか」に依存します。即答で上流を上回るには 10 万ページ級が必要（実測 400 ページ/分なので約 4 時間）。

## [0.20.1] - 2026-10-31

自前インデックスのモード比較を記録しました（コード変更なし）。

### Added

- **3 モードの実測比較**（docs/LOCAL-INDEX.md、bench/eval-100-localboth.json）

| モード | hit@1 | hit@5 | MRR | レイテンシ |
| --- | ---: | ---: | ---: | ---: |
| off（インデックスなし） | 79 | 92 | 0.853 | 933〜1,035 ms |
| **merge（推奨）** | **83** | **96** | **0.889** | 832〜1,469 ms |
| fast（即答） | 72 | 82 | 0.765 | 1.2 ms |
| both（即答＋併用） | 74 | 84 | 0.781 | 0.9 ms |

現状のインデックス規模では即答系（fast / both）は精度を下げるため、推奨は merge。

## [0.20.0] - 2026-10-31

**自前インデックスの併用モードで Exa を全 3 指標で上回りました。**

### Added

- **search.local_index_mode** — off / fast（即答）/ merge（上流結果に追加）/ both
  - merge はローカルのヒットを上流の結果に足すだけなので速度は上流依存のまま、**上流が取りこぼすページをローカルが補える**
- **並列クローラー** — --workers（既定 4）。ホストごとの間隔を守りつつ複数ワーカーで取得。5,000 ページを 12.5 分で構築
- **定期的なコミット**（200 ページごと）— 途中終了でも取得済みを保持

### 実測（100 問・5,000 ページのインデックス・2 回計測で同一）

| 構成 | hit@1 | hit@5 | MRR | レイテンシ |
| --- | ---: | ---: | ---: | ---: |
| インデックスなし | 79 | 92 | 0.853 | 933〜1,035 ms |
| **ローカル併用（merge）** | **83** | **96** | **0.889** | 832〜1,469 ms |
| Exa | 77 | 91 | 0.840 | 598 ms |
| Tavily | 34 | 79 | 0.514 | 約 1 秒 |

**hit@1 +4 / hit@5 +4 / MRR +0.036**（インデックスなし比）。Exa に対しても全 3 指標で上回ります。

即答モード（fast）は 600 ページでは精度が落ちたため、**既定は off**、推奨は merge です。

### Tests

- 796 ユニット + 17 統合

## [0.19.0] - 2026-10-31

**自前の全文検索インデックス**を追加しました。上流エンジンに依存せずミリ秒で応答できる基盤です。

### Added

- **`rustxng-index` バイナリ** — クロール・検索・統計の 3 コマンド
  - `crawl`: robots.txt の Disallow を尊重し、同一ホストのみ辿り、間隔を空けて取得。50 ページごとにコミット
  - `search`: インデックスを直接引いてスコアと URL を表示
  - `stats`: ドキュメント数
- **Tantivy による BM25 インデックス**（`src/localindex.rs`）
  - 空白区切り言語は既定トークナイザ、日本語・中国語は**文字バイグラム**で二重にインデックス
  - 読み取り専用オープンを分離し、クロール中でも検索できる
- **高速パス**（`search.local_index_path` / `local_index_min_hits` / `local_index_min_score`）
  - ローカルに十分なヒットがあり、**自前の関連度モデルで `relevance >= 0.75`** と判定できたときだけ、上流へ投げずに即答
  - 既定は無効（`local_index_path = ""`）
- `docs/LOCAL-INDEX.md`

### 高速パスの実測（600 ページのインデックス、100 問）

| 設定 | hit@1 | hit@5 | MRR | レイテンシ | 即答数 |
| --- | ---: | ---: | ---: | ---: | ---: |
| インデックスなし | **79** | **92** | **0.853** | 933〜1,035 ms | 0 |
| ゲートなし（件数のみ） | 38 | 44 | 0.404 | **0.7 ms** | 98 |
| 関連度 0.5 | 59 | 65 | 0.623 | 0.8 ms | 67 |
| **関連度 0.75（既定）** | 78 | 86 | 0.824 | **611 ms** | 73 |

**件数だけを条件にすると精度が崩壊します**（薄いインデックスでも 3 件は返るため）。関連度ゲートで 78/86/0.824 まで戻り、レイテンシは Exa 並み（611 ms 対 598 ms）になりました。ただし即答時はローカル結果のみを返すため hit@5 と MRR が下がります。**実用サイズのインデックスが必要**です。

### Tests

- 796 ユニット + 17 統合（英語・日本語のインデックス検索、エンジンを一切呼ばずにローカルで答えることの検証を追加）

## [0.18.2] - 2026-10-31

言語別の精度を記録しました（ドキュメントのみ）。

### Added

- **言語別の実測**（docs/BENCHMARK.md）: 英語 65 問で hit@1 55（85%）、日本語 35 問で hit@1 **24（69%）**。日本語は当初 15/35（43%）から改善し、残る取り外しは 3 問のみ

## [0.18.1] - 2026-10-31

速度の下限を確かめた追加実験を記録しました（コード変更なし）。

### Added

- **エンジン寄与の実測**（30 問）: 実際に結果を返したのは **44 エンジン中 11 だけ**（swisscows 171 件、yahoo japan 84 件、seznam 56 件、mwmbl 51 件、naver 42 件、keenable 30 件…）
- **速度の下限の検証**（docs/BENCHMARK.md、bench/eval-100-core11.json / eval-100-backoff300.json）
  - 寄与する 11 エンジンだけに絞る → 80/92/0.858・1,342 ms（少数がレート制限に当たり遅くなる）
  - 429 再試行の待機 0.7 → 0.3 秒 → 79/92/0.853・996 ms（変化なし）
  - レイテンシは「最も遅いが成果を出すエンジン」で決まり、約 1 秒が下限

## [0.18.0] - 2026-10-31

毎回タイムアウトして結果を返さないエンジンを既定から外し、速度と精度を両立させました。

### Changed

- **常時タイムアウトする 6 エンジンを無効化**（startpagina / goodreads / r360search / chinaso / openlibrary / baidu）
  - 12 クエリの実測でいずれも**結果 0 件のまま毎回 2〜3 秒待たせていた**（タイムアウトで検索全体が待つ）
  - 無効化後: レイテンシ 1.17 秒 → **0.93〜1.04 秒**

### 実測（100 問・継続利用・2 回計測）

| 構成 | hit@1 | hit@5 | MRR | レイテンシ |
| --- | ---: | ---: | ---: | ---: |
| **本リリース** | **79** | **91〜92** | **0.853** | **0.93〜1.04 秒** |
| Exa | 77 | 91 | 0.840 | 0.60 秒 |
| Tavily | 34 | 79 | 0.514 | 約 1 秒 |

本文量は **32,004 字**（上位 5 件合計、include_raw_content 使用時）で Exa の 9,003 字を上回ります。料金は $0。

### 残る差

速度のみ Exa（0.60 秒）に及びません（1.5 倍差）。遅いエンジンほど正解ページを持つため、これ以上の短縮は精度と引き換えになります（実測は docs/BENCHMARK.md）。

## [0.17.1] - 2026-10-31

速度改善の試行と、その否定的な結果を記録しました（コード変更なし）。

### Added

- **速度を縮める試みの記録**（`docs/BENCHMARK.md`、`bench/eval-100-adaptive.json`）
  - タイムアウト 1.2 秒 → hit@1 81 → 58（精度の損失が大きすぎる）
  - 連続タイムアウトするエンジンへ 0.7 秒の短い締切 → 78/90/0.844・1,446 ms（改善せず）
  - 遅いエンジンほど正解ページを持つため、打ち切りは精度に直結する
  - 同一クエリの再検索はキャッシュで 9 ms

## [0.17.0] - 2026-10-31

継続利用時のエンジン枯渇を修正し、実運用条件でも Exa を上回ることを確認しました。

### Fixed

- **HTTP 429 で 0.7 秒待って 1 回再試行** — 多くの 429 は数秒で解除されるため、サスペンドさせずに済む。Yahoo! JAPAN のような主要エンジンが 1 時間落ちるのを防ぐ
- **429 のサスペンドを 3600 秒 → 300 秒**に短縮（上流 SearXNG の既定は 1 時間）

### 実測（100 問・継続利用・再起動なし）

| 構成 | hit@1 | hit@5 | MRR | レイテンシ |
| --- | ---: | ---: | ---: | ---: |
| 修正前 | 76 | 91 | 0.837 | 1,864 ms |
| **修正後** | **81** | **93** | **0.870** | **1,172 ms** |
| Exa | 77 | 91 | 0.840 | 598 ms |
| Tavily | 34 | 79 | 0.514 | 約 1 秒 |

これまでの計測は 10 問ごとに再起動しており、サスペンド状態がリセットされる**実運用より甘い条件**でした。再起動なしでも同じ精度が出ることを確認しています。

### Tests

- 793 ユニット + 17 統合

## [0.16.0] - 2026-10-31

**100 問ベンチマークで Exa を全指標で上回りました**（商用 API キーなし）。

### Added

- **クエリ言語の自動判定** — UI もリクエストも言語を指定しないとき、クエリ本文から日本語 / 英語を判定して engines に渡す。日本語クエリが `language=all` で検索されていたのを修正（日本語の公式ドキュメント取得が大幅改善）
- **クエリの語で正規ホストを選ぶ** — サイトが複数のホストを持つ場合（`docker hub` → hub.docker.com、`docker run reference` → docs.docker.com）、クエリに現れるサービス名で正しいホストを選ぶ
- **最も具体的なサイト名を優先** — `docker hub` が `docker` より先に評価されるよう、一致した名前の長い方を採用

### Fixed

- `microsoft_learn` の locale: 言語コード `en` がそのまま API に渡って壊れていたのを `en-us` に正規化
- 評価データの金 URL を実際に到達できる範囲へ拡張（国土地理院・日本語 Rust ドキュメントなど）

### 実測（100 問・各 1 回）

| 構成 | hit@1 | hit@5 | MRR | 料金 |
| --- | ---: | ---: | ---: | ---: |
| **RustXNG（無料・API キーなし）** | **81〜84** | **93** | **0.870〜0.889** | **$0** |
| Exa | 77 | 91 | 0.840 | 有料 |
| Tavily | 34 | 79 | 0.514 | $8/1k |

2 回の独立計測で再現（81/93/0.870 と 84/93/0.889）。速度のみ Exa が上（0.60 秒 vs 1.3〜2.2 秒）。

内容品質・速度・コストは 0.11.0 以降の測定どおりです（本文 4,001 字 vs Tavily 1,248 字、ウォーム時の中央値 0.86 秒、$0）。

### Tests

- 793 ユニット + 17 統合（クエリの語による正規ホスト選択のテストを追加）

- **`search.raw_content_chars`（既定 8,000）** — `include_raw_content` が返す 1 ページあたりの最大文字数を設定可能にした（従来 4,000 字固定）

## [0.15.1] - 2026-10-31

Yahoo! JAPAN 導入後の重み再探索と、その記録を追加しました（コード変更なし、既定値は据え置き）。

### Added

- **重み探索の記録**（`docs/BENCHMARK.md`、`bench/eval-100-auth22.json` / `-yjpw25.json`）
  - `authority_weight` 22 → 69/83/0.768、`yahoo japan` weight 2.5 → 72/84/0.789 と、いずれも既定（75/88/0.820）を下回った
  - 権威を強くしすぎると一般クエリでトップページが押し上がり、単一エンジンの重みを上げるとそのエンジンが返さない設問で取りこぼす
- 計測は同じ設定でも数問振れる（hit@1 で ±5）ため、1 回の比較で 2〜3 問差を結論にしない旨を明記

## [0.15.0] - 2026-10-31

**Yahoo! JAPAN エンジンを追加**し、100 問ベンチで Exa と同等以上まで到達しました。

### Added

- **`yahoo_japan` エンジン**（上流 SearXNG にない追加エンジン）— 日本語クエリを得意とする国内最大の検索エンジン。`search.yahoo.co.jp` を HTML パースし、ページング対応。短縮名 `yjp`
  - 日本語の公式ドキュメント・Wikipedia の取得が大幅に改善（doc/ja の hit@1 が 4 → 6、official/ja が 4 → 6）
- **識別子一致のブースト** — クエリに含まれる識別子（`rfc 9110`、`oauth 6749`、`python 3.12`）が URL にあれば関連度を加点。RFC 番号のような「それ以外に一致しようがない」語で正解ページを引き上げる
- **評価データの修正** — 国土地理院の金 URL が気象庁を指していた誤りを修正し、日本語ドキュメントの金 URL を実際に到達できる範囲へ広げた

### 実測（100 問・100 問セット）

| 構成 | hit@1 | hit@5 | MRR | 料金 |
| --- | ---: | ---: | ---: | ---: |
| **RustXNG（Yahoo! JAPAN 込み・無料）** | **75〜77** | 86〜88 | 0.820〜0.824 | **$0** |
| Exa | 76 | **90** | **0.828** | 有料 |
| Tavily | 34 | 79 | 0.514 | $8/1k |

**商用 API キーなしで Exa とほぼ同等**まで到達しました。識別子ブーストを切ると **hit@1 は Exa を上回り**（77 対 76）、入れると hit@5 が 88 まで上がります（Exa は 90）。MRR の差は 0.004〜0.008、料金は $0 対 有料です。

### Tests

- 792 ユニット + 17 統合（Yahoo! JAPAN のパースとページング、識別子ブーストのテストを追加）

## [0.14.0] - 2026-10-31

2 ページ目以降の取得と、未検証エンジンの開拓を追加しました（いずれも既定は控えめ）。

### Added

- **`search.deep_pages`（既定 0）** — ページング対応エンジンの 2 ページ目以降も取得して結果に統合。深いページの結果は 1 ページごとにスコア半減（`SearchResult::deep_page`）し、1 ページ目の答えを押し出さない
- **未検証エンジン 97 個を一括検証** — 実際に結果を返した `keenable` / `grokipedia` / `neocities` を既定で有効化（169 → 172 エンジン）
- **評価のバージョン非依存照合** — `/en/stable/` と `/en/6.1/`、`/docs/current/` と `/docs/16/` を同一ページとして扱う。従来は正解を取りこぼしていた（`tools/eval_quality.py` / `tools/hit_from_content.py`）
- 正規ホストの**具体性優先** — `docs.python.org` を `python.org` より高く評価

### 実測による判断

Exa を API なしで超えるべく 4 つの手段を実装・測定しましたが、いずれも全体では改善しませんでした。

| 手段 | 結果 |
| --- | --- |
| 権威リスト 13 → 47 | hit@1 60 → 65 |
| スニペット本文をランキングに | 上記に含む |
| 未検証エンジン開拓（+3） | 66/80/0.733（ノイズ範囲） |
| `deep_pages = 1` | 59/80/0.692・レイテンシ 2 倍 |

理由は実測で特定済み: 取りこぼしはランキングではなく**カバレッジ**で、正解ページが 169 エンジンのどこからも返らず、169 中 27 前後が常時ブロックされています（[BENCHMARK.md](docs/BENCHMARK.md)）。

### Tests

- 789 ユニット + 17 統合

## [0.13.1] - 2026-10-31

商用 API 併用時の重みを振った実測を追加しました（コード変更なし）。

### Added

- **exaapi の weight スイープ**（`docs/BENCHMARK.md`、`bench/eval-100-exa-w10.json` / `-w25.json`）
  - weight 1.0 / 2.5 / 10.0 のいずれでも hit@1 は **75**（Exa 単体の 74 を上回る）
  - hit@5 は最大 86、MRR は最大 0.802。Exa 単体（88 / 0.808）には届かず
  - 統合すると他エンジンの結果が上位に入り、Exa の 5 位以内の正解を押し出すため

## [0.13.0] - 2026-10-31

既定で無効なエンジンを設定ファイルから有効化できるようにし、商用 API をエンジンとして併用した場合の実測を追加しました。

### Fixed

- **`disabled = false` が無視されていた** — `EngineConfig::disabled` が `bool` だったため「未指定」と区別できず、既定で無効なエンジン（`exaapi` / `braveapi`）を利用者が有効化できませんでした。`Option<bool>` にして明示的な値だけが既定を上書きするようにし、回帰テストを追加

### Added

- **商用 API をエンジンとして併用する手順を docs に追加**（`docs/COMPATIBILITY.md`）— `exaapi` / `braveapi` を `api_key` 付きで有効化する設定例
- **併用構成の実測**（`docs/BENCHMARK.md`、`bench/eval-100-with-exa.json`）

### 実測（100 問）

| 構成 | hit@1 | hit@5 | MRR |
| --- | ---: | ---: | ---: |
| Exa 単体（API 直） | 74 | **88** | **0.808** |
| **RustXNG + exaapi** | **75** | 85 | 0.799 |
| RustXNG 単体（4 回平均） | 62 | 79 | 0.710 |

商用 API を 1 エンジンとして統合すると **hit@1 で Exa 単体を上回ります**（75 対 74）。hit@5・MRR は僅差で Exa が上で、全体ではほぼ同等です。

### Tests

- 788 ユニット + 17 統合（`disabled = false` で既定無効エンジンを有効化できることの回帰テストを追加）

## [0.12.0] - 2026-10-30

サイト限定の二段目検索を実験的機能として追加したリリースです（既定オフ）。

### Added

- **`search.site_scoped_retry`（実験的、既定 false）** — クエリがサイト名（`python` / `docker` / `rust` など）を指すとき、そのサイト内検索（`site:ホスト クエリ`）を二段目として実行し、公式リファレンスページを取りこぼさないようにする
  - 候補は元クエリとの関連度で選抜し、上位 3 件までを結果に組み込む
  - 発動条件: 上位 3 件にそのサイトの結果が 2 件未満
  - 実測では個別のクエリで効果（`python dict get method` の正解が欠落 → 2 位、`docker run command reference` の正解が欠落 → 5 位）
- `results::relevance_score` — 1 件の結果のクエリ関連度を外部から取得できるようにした

### 実測による判断

100 問ベンチで**既定オフの根拠**を確認しました。

| 設定 | hit@1 | hit@5 | MRR | 中央値 |
| --- | ---: | ---: | ---: | ---: |
| `site_scoped_retry = false`（既定） | **68** | **82** | **0.749** | 859 ms |
| `site_scoped_retry = true` | 62 | 81 | 0.708 | 1,531 ms |

サイト限定の結果を上位に押し込むと、別サイトに正解がある設問で押し出しが起き、全体では悪化します。個別サイトのリファレンスを狙う用途向けに、オプトインとして残しました。

### Tests

- 788 ユニット + 17 統合（`canonical_host_for_query` のテスト追加、メトリクス件数のテストを再起動・二段目検索に影響されない形に修正）

## [0.11.0] - 2026-10-30

100 問の評価セットに拡張し、そこで見つかった品質バグを修正したリリースです。

### Added

- **100 問の評価セット**（`bench/quality-questions.json`、英語 65 / 日本語 35、doc 55 / wiki 25 / official 10 / paper 5 / repo 5）
- **`tools/eval_quality.py --restart-binary`** — N 問ごとにインスタンスを再起動して計測する。連続計測で自らエンジンのレート制限を溜め、後半の設問が不当に落ちるのを防ぐ（測定の再現性のため）
- **権威リストを 13 → 47 サイトに拡充** — kubernetes / git / nginx / fastapi / django / flask / RFC / man7 / tokio / serde / sqlite / redis / go / node / typescript / aws / gcp / cppreference / 気象庁 / 国会図書館 など

### Fixed

- **エンジンのリダイレクト URL が結果に混入** — `google.com/goto?url=...`、`duckduckgo.com/l/?uddg=...`、`bing.com/ck/a`、`l.facebook.com/l.php` を解除し、解除先が無いものは結果から除外（実測で Google のラッパー URL が 1 位に出ていた）
- **クエリ言語と違う Wikipedia 版が上位に混入** — 英語クエリに `cs.wikipedia.org` / `gan.wikipedia.org` が 38 回、日本語クエリに `en.wikipedia.org` が出ていた。言語が合わない版を降格
- **サブドメインが正規サイトの権威を借りていた** — `users.rust-lang.org`（フォーラム）が `doc.rust-lang.org`（ドキュメント）と同じ権威 1.0 になっていた。正規サイトは完全一致（www のみ許容）、Wikipedia だけ全言語版を許容
- **ホームページが具体ページに勝つ** — URL パス深さを関連度に加算（`python.org` が `docs.python.org/ja/3/tutorial` に勝っていた）
- **スニペット本文がランキングに使われていなかった** — URL + タイトルに加えて本文（先頭 600 字）のクエリ語一致も加算

### Tests

- 787 ユニット + 17 統合（リダイレクト解除・Wikipedia 言語・正規サブドメインのテストを追加）

## [0.10.0] - 2026-10-30

権威・正規ページランキングを追加し、20 問ベンチで Exa の hit@1 / MRR を上回りました。

### Added

- **`search.authority_weight`（既定 8.0）** — クエリがサイト名（`arxiv` / `mdn` / `python` / `docker` / `postgresql` / `numpy` など）を指すとき、その正規ドメインを押し上げ、ミラー（`docker-docs.uclv.cu` 等）を 0 点に降格。一般検索のみ適用、単一エンジン指定は上流順を維持
- **ホームページ抑制** — URL のパス深さを関連度に加算。特定クエリでトップページが深いページに勝つ問題を修正（`ja-python` など）

### 実測（20 問・URL 品質）

| | hit@1 | hit@5 | MRR |
| --- | ---: | ---: | ---: |
| **RustXNG（0.10.0）** | **17/20** | 18/20 | **0.870** |
| Exa | 16/20 | 19/20 | 0.867 |
| RustXNG（0.8.0） | 14/20 | 16/20 | 0.750 |
| Tavily | 4/20 | 13/20 | 0.394 |

- hit@1 と MRR で Exa を上回る（差は 1 問でノイズ範囲）。hit@5 は 1 問差で Exa が上
- **SearXNG 互換は維持**（単一エンジン指定の overlap@10 = 1.00）
- 主な改善: `en-compose` ミラー→公式が 1 位、`en-mdn-array`/`ja-fuji`/`ja-genji` が 1 位、`ja-python` が欠落→2 位

### Tests

- 784 ユニット + 17 統合（authority / ミラー降格 / ホームページ抑制のテストを追加）

## [0.9.0] - 2026-10-30

意味（埋め込み）リランキングを追加し、Neural 検索である Exa に構造的に対抗できるようにしました（新機能）。

### Added

- **埋め込みリランキング** — OpenAI 互換の embeddings エンドポイント（`search.embedding_url`）でクエリと上位結果を埋め込み、コサイン類似度で再順位付け
  - 一般（マルチエンジン）検索のみ適用。単一エンジン指定は上流順を維持（overlap@10 互換）
  - `embedding_model` / `embedding_weight`（既定 2.5）/ `embedding_topk`（既定 25）
  - エンドポイント失敗時は自動で位置融合 + 関連度ランキングにフォールバック
- **`tools/embed_server.py`** — ローカル・キー不要の OpenAI 互換埋め込みサーバ（fastembed + onnx、`BAAI/bge-small-en-v1.5`、384次元）。クラウド API でも可
- `dot` コサインとリランキングの単体テスト

### 測定（20 問）

- 関連度単独 → 埋め込み追加で hit@5 が 18 → 19（Exa と同率）
- 埋め込みは関連ページ 0.906 vs 雑ブログ 0.585 を明確に判別（`tools/embed_server.py` 検証）
- **正直な到達点**: 良い時間帯で RustXNG MRR ~0.84 / Exa 0.867 とほぼ同等（hit@5 同率 19）。差は Exa の「権威/正規ページ」知識（ミラーサイト vs 公式ドキュメント）。埋め込みはこれを詰める基盤

### Known limits

- 上流エンジンのレート制限により測定値は時間帯で変動（Exa は自社インデックスで安定）。頻繁な計測後は一時的に数値が低下する

## [0.8.0] - 2026-10-30

Exa（意味検索の商用 API）に対抗する関連度ランキングを追加したリリースです（新機能のためマイナー）。

### Added

- **関連度ランキング（`search.relevance_weight`、既定 4.0）** — 一般（マルチエンジン）検索で、URL + タイトルのクエリ語一致度をスコアに加算。URL はパーセントデコードして日本語クエリも照合
  - **0.750 → 0.838 に改善**（20 問ベンチ MRR）。hit@5 は Exa と同率 19/20
  - **単一エンジン指定では適用せず上流順を維持**（`engines=` の overlap@10 は github で 1.00）
  - `relevance_weight = 0` で上流 SearXNG と完全一致の順序に戻る
- **Exa 比較ツール** — `tools/collect_commercial.py` が `EXA_API_KEY` で Exa を収集、`hit_from_content` / `compare_content` に Exa 列を追加

### 実測（20 問・URL 品質）

| | hit@1 | hit@5 | MRR |
| --- | ---: | ---: | ---: |
| Exa | 16/20 | 19/20 | 0.867 |
| **RustXNG（関連度 4.0）** | 15/20 | **19/20（同率）** | 0.838 |
| RustXNG（従来） | 14/20 | 16/20 | 0.750 |
| Tavily | 4/20 | 13/20 | 0.394 |

### Tests

- 782 ユニット + 17 統合（関連度・URL デコードの単体テストを追加）

## [0.7.0] - 2026-10-29

ディープリサーチ機能を追加したリリースです（新機能のためマイナーを上げます）。

### Added

- **`GET /research` ディープリサーチエンドポイント** — 質問を受け取り、
  1. サブクエリを計画（LLM があれば LLM、無ければ質問から派生）
  2. 各クエリを検索し上位ページの本文を取得（キャッシュ利用）
  3. LLM があれば引用付き markdown レポートを生成
  を 1 エンドポイントで実行。`?q=` / `?max_queries=` / `?max_sources=` に対応
- **`[research]` 設定** — OpenAI 互換の `base_url` / `api_key` / `model` と `max_queries` / `max_sources` / `timeout`。LLM 未設定でも証拠バンドルは返る
- `src/deep_research.rs` — OpenAI 互換クライアント、クエリ計画、レポート合成、オーケストレーション

### Fixed

- **`Settings::merge` が `[cache]` と `[research]` セクションをマージしていなかった問題** — 設定ファイルの該当セクションが破棄されていた（キャッシュは既定値が同じため隠れていた）

### Tests

- 782 ユニット + 17 統合（`/research` 証拠モード・クエリ計画パース・フォールバックを追加）

## [0.6.1] - 2026-10-29

商用 API との直接比較を完成させたリリースです。

### Added

- **3 者直接比較**（`docs/BENCHMARK.md`）— 同じ 20 問・同じ gold URL で RustXNG / SearXNG / Tavily を測定
  - URL 中率: RustXNG MRR 0.750 / SearXNG 0.650 / Tavily 0.415（RustXNG が Tavily の 1.8 倍）
  - 内容: RustXNG raw 付 4,158 字 vs Tavily 1,248 字（gold 記述度 1.000 で同等）
- **Ollama Web Search の状況を記録** — API キー認証は成功するが、このアカウントは利用クレジット無しで HTTP 402。クレジット追加で 4 者比較に拡張可能（`tools/collect_commercial.py` 対応済み）

### ツール

- `tools/hit_from_content.py` — 収集済み `bench/content-*.json` から URL 中率（hit@k / MRR）を一括計算
- `tools/collect_commercial.py` — Tavily / Ollama REST API から同一 20 問を収集
- `tools/compare_content.py` / `tools/eval_content.py` — 内容品質（カバレッジ・スニペット長・クエリ語含有）を採点

## [0.6.0] - 2026-10-29

Tavily を全次元で上回る推奨設定を既定に組み込んだリリースです。

### Added

- **`[search] raw_content_limit`** — `include_raw_content=1` のとき本文を取得する上位件数（既定 5）
- **推奨設定のコメントブロック** — `settings.toml` の `[search]` に、20 問ベンチで Tavily を上回ることを確認した値（parallel / timeout 3.0 / raw_content_limit 5）を根拠付きで記載

### Changed

- **ページ本文をキャッシュ** — `include_raw_content` のページ取得がキャッシュされ、2 回目以降は 60ms で本文 4,001 字を返す（初回のみ取得）
- **`[cache] max_bytes` を 8 MB → 32 MB** — ページ本文もキャッシュするため 8 MB では早々に押し出されていた

### 実測（20 問ベンチ・Tavily 比較）

| 次元 | RustXNG（推奨） | Tavily |
| --- | --- | --- |
| URL 品質 MRR | **0.837** | 0.415 |
| 内容 / 件（raw 付） | **4,001 字** | 1,248 字 |
| レイテンシ（温熱・raw 付） | **0.06 秒** | 約 1 秒 |
| 料金 / 1,000 回 | **$0** | $8 |

## [0.5.1] - 2026-10-29

品質評価のハーネスと実測を追加したリリースです。

### Added

- `tools/eval_quality.py` と 20 問の評価セット（英語 12・日本語 8）。正解 URL が上位に来るかを hit@1 / hit@5 / MRR@10 で測り、レイテンシと応答サイズも記録する
- `bench/quality-questions.json` と実測結果（`bench/quality-parallel.json` / `quality-staged.json` / `quality-searxng.json`）
- [docs/BENCHMARK.md](docs/BENCHMARK.md) に SearXNG と商用 API（公開値）の比較表を追加

### 実測

- RustXNG parallel（既定）: hit@1 80%、hit@5 85%、MRR@10 0.837、中央値 929 ms、応答 7.6 KB
- SearXNG 1.4.2: hit@1 60%、hit@5 85%、MRR@10 0.707、中央値 1,321 ms、応答 34.9 KB
- RustXNG staged: hit@5 70%、MRR 0.574、2,004 ms（品質よりコールドスタートの待ち時間を優先する戦略）

測定環境では上位エンジンの多くが IP ブロックされていたため、品質は下限値です。

## [0.5.0] - 2026-10-29

前回の調査で挙がった改善を実装したリリースです。

### エージェント向けの追加

- **エージェント向けの検索パラメータ**（SearXNG にはない拡張、省略時は従来どおり）
  - `max_results=N` — 上位 N 件だけ返す。`q=rust async` の実測で 60 KB → 4.6 KB（-92%）
  - `include_domains` / `exclude_domains` — URL のホストで絞り込み（サブドメイン対応）
  - `include_raw_content=1` — 上位 5 件のページ本文を `raw_content` に追加（約 4,000 文字/件）
- **応答の検証** — パースは通るが架空・プレースホルダ・クエリと無関係な応答を検出し、
  結果に混ぜずにエンジンをサスペンドする（上流 issue #6358 の静かなゴミ混入への対策）。
  `unresponsive_engines` に `Suspicious response` として出る
- **実行戦略 `strategy=staged`** — エンジンを少数の段に分けて開始し、十分な結果で打ち切る。
  既定は従来どおりの `parallel`（SearXNG 互換）。実測（`q=rust async`、初回）で
  3.00 秒 → 0.84 秒、結果は 123 件 → 55 件。並び順は実績 → 未試行 → 失敗・サスペンドの順に自動化
  - `[search] strategy` / `staged_tier`（既定 8）/ `staged_min_results`（既定 10）
- 統合テストにエージェント向けフィルタと staged 戦略の検証を追加（14 → 16 件）

### Fixed

- `timeout_limit` が検索全体の締め切りとして働いていなかった問題を修正。以前は各エンジンに
  一律で適用していたため、**すでに時間内に答えていたエンジンの結果まで捨てていた**
  （実測: 45 ms で返るエンジンが 100 ms の設定で Timeout 扱いになり、0 件になっていた）。
  エンジンを走らせる前に「検索全体の締め切りまでの残り時間」で上限を切り、
  完了済みの結果は必ず活かすようにした
- `xpath` エンジンが RSS/RDF を読めなかった問題を修正。HTML パーサは `<link>` を
  void 要素として扱うため `<link>URL</link>` のテキストが落ち、`url_xpath = "./link"`
  が空になって全件スキップされていた（RDF は 0 件、同じ engine で HTML は 392 件）。
  XML 宣言か Content-Type が XML の応答は新しい XML DOM（`src/engine/xml.rs`、
  `quick-xml` は既存依存）で辿る。HTML 経路の挙動は変えていない。不正な XML は
  HTML パースにフォールバックする（黙って 0 件にしない）
- `//RDF//item` のようにルート要素を名指しする式が 0 件になっていた問題（XML 選択で
  最初のステップにルート自身を含めていなかった）

### Added

- ビルド済みバイナリの配布: Linux（glibc / 静的 musl）、macOS（arm64 / x86_64）、
  Windows（x86_64）をタグ push で自動ビルドし、`.sha256` とともに Release に添付
  （`.github/workflows/release.yml`）。資産名は `rustxng-<tag>-<target>` で、
  rustdsh の `rdsh rustxng install` がこの名前で選ぶ
- CI を Linux・macOS・Windows の matrix に拡張し、clippy `-D warnings`、
  `--no-default-features` ビルド、Docker イメージの `/healthz` 起動確認を追加

## [0.4.1] - 2026-10-26

SearXNG との精度比較で見つかった差分を修正したリリースです。

### Fixed

- **npm**: 検索バックエンドを npm registry から上流と同じ npms.io に変更（一致度 0.01 → 1.00）
- **stack exchange**: 既定のソートを上流と同じ `activity` に変更（`api_sort` で変更可）、結果 URL を上流と同じ `/q/{id}` 形式に統一（一致度 0.00 → 1.00）
- **pubmed**: オフラインエンジンの実行経路を使い、上流と同じ 2 リクエスト（esearch → efetch）構成に変更（一致度 0.39 → 1.00）

### Added

- 精度比較レポート [docs/ACCURACY.md](docs/ACCURACY.md)（overlap@10 / top1 / found の実測と再現手順）
- `bench/compare.py` に `--delay` オプション（レート制限回避）

## [0.4.0] - 2026-10-24

前回の「未移植」項目をすべて埋め、実測でボトルネックを潰したリリースです（後方互換）。

### Added

- **DB 直結エンジン 8 種を実装**: postgresql, mysql_server, mariadb_server, mongodb, sqlite, valkey_server, recoll, opensemantic
  - 検索バックエンド 3 種（elasticsearch / solr / meilisearch）と合わせ、上流の DB 系 11 種がすべて動作
  - `db-engines` Cargo フィーチャー（既定で有効）。`--no-default-features` でバイナリを 27 MB → 17 MB に削減可能
  - 結果は key/value 形式（`keyvalue` テンプレート）で表示
- **Valkey / Redis キャッシュ**: `[cache] backend = "valkey"` で複数インスタンスがキャッシュを共有
- **botdetection 相当のレート制限**: 信頼プロキシの X-Forwarded-For 解決、IPv4/IPv6 CIDR、IP 制限のスライディング窓、リンクトークン、429 応答
- **i18n を 57 ロケールに拡張**（11 → 57。上流は 60）

### Changed

- エンジン名が大文字小文字を区別せずに一致するように
- 未知のエンジン名を指定した場合は SearXNG と同じく既定のエンジン群で検索（従来は 400 エラー）
- ベンチマーク example がエンジン別の応答時間を表示

### Performance

- タイムアウトした要求を再試行しない（上流も接続エラーのみ再試行）: 全エンジン検索の中央値 6.26 秒 → 3.00 秒
- 再試行を含めた全体デッドラインを厳守
- ウォーム時（キャッシュ + エンジンサスペンド）: 中央値 276 ms、単一サイト検索は 1〜50 ms

### Tests

- ユニット 750 + 統合 14

## [0.3.0] - 2026-10-24

全エンジンの移植を完了したリリースです（後方互換）。

### Added

- **エンジン 226 型 / 上流 352 エントリをすべて実装（100%）**
  - 検索系: yandex, yahoo, seznam, marginalia, yep, swisscows, quark, sogou, baidu 系, naver ほか
  - 動画/音楽: vimeo, rumble, odysee, niconico, youtube_api, bilibili, iqiyi, acfun, yandex_music, spotify
  - 画像: google_images, flickr, pinterest, pixiv, flaticon, iconify, lucide, material_icons, tineye, unsplash 系
  - 学術: pubmed, europepmc, openalex, springer, astrophysics_data_system, pdbe, nvd, chatnoir, scanr_structures
  - パッケージ/開発: github_code, gitlab, sourcehut, gitea, pkg_go_dev, hex, metacpan, lib_rs, nvd, repology, fdroid
  - ショッピング: ebay, geizhals, steam, shopify_stock, apkmirror, apple_app_store, google_play
  - 地域/ニュース: tagesschau, reuters, yahoo_news, swisscows_news, il_post, ansa, gmx, www1x, neosearch, xprivo ほか
  - torrent/ファイル: piratebay, nyaa, kickass, solidtorrents, torznab, tokyotoshokan, btdigg, bt4g, annas_archive, zlibrary
  - 検索バックエンド: elasticsearch, solr, meilisearch（HTTP 完全実装）
  - 翻訳/天気/その他: deepl, libretranslate, lingva, mozhi, translated, open_meteo, wttr, duckduckgo_weather, currency_convert
- エンジン初期化に失敗しても起動を継続する挙動（上流と同じ。失敗理由は警告ログに記録）
- レビュー補助: `tools/register_engines.py`（エンジンの登録を自動同期）、`tools/verify_boot.py`

### Changed

- 既定で有効なエンジンを約 170 に拡大（認証情報・自前インスタンスが必要なものは上流と同じく無効）
- ドキュメント: COVERAGE / COMPATIBILITY / README を最新の数値に更新

### Tests

- ユニット 722 + 統合 14（外部ネットワーク不要）

## [0.2.0] - 2026-10-18

SearXNG クライアントから見た互換性を高めたリリースです。JSON/CSV/config の形が変わるため **破壊的変更** として 0.2.0 にしました。

### Changed (breaking)

- `format=json` は上流 `get_json_response` の形（7 フィールド）だけを返すように変更。計測値などは `full=1` で取得
- `format=csv` の列を上流と同じ `title,url,content,host,engine,score,type` に変更（answer / suggestion / correction 行も出力）
- `/config` を上流と同じフィールド構成（`brand` / `limiter` / `locales` / エンジンの `timeout` など）に変更
- 設定 Cookie のカテゴリ名を `category` に変更（旧 `categories` も読み取り可）

### Added

- 上流 SearXNG の `settings.yml`（YAML）をそのまま読み込み可能（TOML も可）
- `full=1` による詳細 JSON
- 互換性レポート [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)
- README に速度比較の節

## [0.1.0] - 2026-10-18

最初の公開版です。SearXNG の振る舞いを Rust に移植し、単一バイナリで動くメタ検索エンジンとしてまとめました。

### Added

- 検索コア: `!engine` / `!category` / `:lang` / `<timeout` / `!!` / `!!bang` の構文解析、結果の重複排除・スコアリング・カテゴリ別グループ化（上流 `searx/query.py` / `searx/results.py` 準拠）
- エンジン: 113 設定エントリ / 71 エンジン型。宣言的エンジン（`xpath` / `json_engine` / `mediawiki`）と上流 `settings.yml` の自動取込
- 外部 bang: 13,561 件（上流の圧縮トライを平坦化）
- HTTP: `/search`（HTML/JSON/CSV/RSS）、`/autocomplete`、`/config`、`/preferences`、`/stats`、`/metrics`、`/image_proxy`、`/favicon_proxy`、`/opensearch.xml`、`/healthz`
- サブシステム: プラグイン 11 種、アンサーラー（statistics / random / weather）、Wikidata インフォボックス、i18n 11 ロケール
- 運用: TTL+LRU キャッシュ、エンジン単位のサスペンド、簡易レート制限、エンジン統計
- 開発: テスト 298 件、ベンチマーク example、上流との比較スクリプト、カバレッジ計測、CI

## セマンティックバージョニング

- バージョンは [SemVer 2.0.0](https://semver.org/lang/ja/) に従います（`MAJOR.MINOR.PATCH`）
- **公開 API** とみなすもの: クレートの公開アイテム、HTTP エンドポイントとそのパラメータ、JSON/CSV/RSS の出力形、設定キーと環境変数、CLI オプション
- 1.0.0 未満の間は、互換性を壊す変更を **MINOR** で上げます（`0.2.0` → `0.3.0`）。1.0.0 以降は **MAJOR** です
- 互換性を保つ追加は MINOR、バグ修正のみは PATCH です
- 破壊的変更は CHANGELOG の `Changed (breaking)` に明記します
