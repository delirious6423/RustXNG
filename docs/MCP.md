# MCP サーバー

`rustxng mcp` は、検索を [Model Context Protocol](https://modelcontextprotocol.io/) のツールとして stdio で提供します。
公式 Rust SDK [`rmcp`](https://crates.io/crates/rmcp) を使い、HTTP API と同じ設定・エンジン・キャッシュ・サスペンド状態を共有します。
ネットワークのリスナーは開きません。

## 起動

```bash
rustxng mcp                         # 設定は $SEARXNG_SETTINGS_PATH または既定
rustxng mcp --config settings.toml
```

ログは stderr に出ます。stdout は MCP のメッセージ専用です。

クライアントの設定例（Claude Desktop・Claude Code・Cursor などの `mcpServers` 形式）:

```json
{
  "mcpServers": {
    "rustxng": {
      "command": "/home/you/.local/bin/rustxng",
      "args": ["mcp"]
    }
  }
}
```

## ツール

どちらも読み取り専用です（`readOnlyHint`）。引数は JSON Schema で検証され、未知の項目は拒否されます。
返す内容は Web 由来の信頼できないデータで、各応答の `note` にもそう明記します。

### `search`

| 引数 | 既定 | 説明 |
| --- | --- | --- |
| `query` | 必須 | 検索語。`!wp` などの bang も使えます（外部サイトへの bang はエラー） |
| `limit` | 5 | 件数（1〜10） |
| `categories` | general | `news`、`it`、`science` など |
| `time_range` | なし | `day` / `week` / `month` / `year` |
| `language` | 設定の既定 | `en`、`ja` など |
| `include_domains` / `exclude_domains` | なし | サブドメインを含めて絞り込み・除外 |

順位・URL・タイトル・スニペット（最大 500 文字、`snippet_truncated` で打ち切りを明示）・エンジン・日付と、応答しなかったエンジンを返します。
ページ本文は含めません。

### `read_page`

| 引数 | 既定 | 説明 |
| --- | --- | --- |
| `url` | 必須 | 同じセッションの `search` が返した URL |
| `offset` | 0 | 開始位置（Unicode 文字） |
| `max_chars` | 4000 | 文字数（1〜20000） |

ページの本文（`include_raw_content` と同じ抽出）を区切って返し、続きがあれば `next_offset` を返します。
`search` が返していない URL は読みません（任意の URL の取得はできません）。
検索結果は第三者のデータなので、接続先はグローバルなアドレスに限ります（IPv4/IPv6 のループバック・プライベート・リンクローカルなど、ホスト名の解決結果、リダイレクト先のすべてを検査）。共有のレスポンスキャッシュは使わず、本文は 4 MiB まで、同時取得は 4 件までです。1 ページで読めるのは先頭 200,000 文字までで、抽出した本文はセッション内で 8 ページまで保持するため、続きを読むときに再取得しません。保持から外れたページは取得し直すため、その間にページが変わっていれば `offset` の位置もずれます。

## 使い方の目安

1. `search` で候補を少数（`limit` 3〜5）取得する
2. 必要な結果だけ `read_page` で読む。長いページは `next_offset` で続きを読む

本文を最初から全件返す `include_raw_content=1` より、モデルに渡す量を抑えられます。
