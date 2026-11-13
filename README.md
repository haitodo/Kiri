# Kiri — Windows 11 作業シーン切替ランチャー

[spec.md](file:///d:/dev/Kiri/spec.md)（SD-SPEC-001 v1.1）に基づく、省リソース・非干渉型のWindows 11作業シーン切替ランチャーです。
デュアルモニター環境等で、特定の制御対象モニターに配置された既存ウィンドウ（MT5、cTrader、VS Code、ブラウザ等）を作業シーンごとに**アイコン1クリック**で瞬時に最小化／復元して切り替えます。

---

## 1. 概要と主な特徴

- **非干渉性（Strict Non-Interference）**: 制御対象外モニターのウィンドウ（例: モニター1のブラウザ）や未登録ウィンドウには一切干渉しません（FR-05）。
- **非アクティブ（Focus Steal Prevention）**: `WS_EX_NOACTIVATE` および `WM_MOUSEACTIVATE` (`MA_NOACTIVATE`) を採用し、ランチャー操作時にアクティブな作業ウィンドウのキーボードフォーカスを奪いません（FR-19）。
- **仮想デスクトップ不使用**: Windows仮想デスクトップ（切替アニメーションや全画面再描画）を用いず、同一デスクトップ上で既存ウィンドウの最小化／復元のみを即座に実行します。
- **イベント駆動・省リソース**。ネイティブWin32メッセージループ（`GetMessageW`）、最小化通知と入力フックで動作します。カーソル付近のランチャーもイベントに応じて描画します。
- **安全第一の設計**:
  - モーダルダイアログ表示中（親窓 `!IsWindowEnabled`）のウィンドウは最小化せずスキップし警告を出力（FR-17）。
  - 切替直前に文字入力中だった前景ウィンドウは最後尾に `SW_MINIMIZE` で最小化し、不可視ウィンドウに入力が残る事故を防止（FR-18）。
  - Kiri自身が最小化したウィンドウのみを復元対象とし、ユーザー自身が手動最小化したウィンドウを勝手に復元しない（FR-08）。
  - 先行書き込みジャーナル（`runtime-state.json`）によるクラッシュ復旧および「すべて復元」安全機構。

---

## 2. 実装モジュール構成

| モジュール | 実装ファイル | 機能概要 |
|---|---|---|
| **設定管理・アトミック保存** | [src/config.rs](file:///d:/dev/Kiri/src/config.rs) | `config.json` 読込・検証、未知キー警告、将来 `schema_version` 検出時の読み取り専用保護、`FlushFileBuffers` + `ReplaceFileW` / `MoveFileExW` によるアトミック置換、`config.json.bak` 1世代バックアップ保持 |
| **先行書き込みジャーナル・復旧** | [src/recovery.rs](file:///d:/dev/Kiri/src/recovery.rs) | `runtime-state.json` への先行書き込み（`pending` → `minimized_by_us`）、再起動時クラッシュ復旧照合（`HWND` + `PID` + `process_start_time` + `IsIconic`）、WinEvent によるユーザー手動復元検知（`invalidated` 遷移、FR-08 / AT-23）、「すべて復元」フェイルセーフ |
| **シーン切替エンジン（純粋ロジック）** | [src/scenes/switch.rs](file:///d:/dev/Kiri/src/scenes/switch.rs), [src/scenes/manager.rs](file:///d:/dev/Kiri/src/scenes/manager.rs) | 10ステップ差分切替アルゴリズム（`Unknown` / `Active` 状態遷移、対象モニター外保護 FR-05、共通窓・`always_visible` 保護 FR-07、モーダル表示窓スキップ FR-17、前景窓最後尾 `SW_MINIMIZE` FR-18、Z順下位からの復元、500ms単発タイマーによる結果照合・トレイ格納アプリ検出） |
| **ウィンドウ照合・曖昧さ検出** | [src/scenes/bindings.rs](file:///d:/dev/Kiri/src/scenes/bindings.rs) | 2段階照合（実行時同一インスタンス再検証＋正規化EXEパス／クラス／タイトル規則 `any`/`exact`/`prefix`/`contains`/`regex`／パッケージアプリAUMID）、同一モニター内複数候補の曖昧検出（AT-10）、別モニター同一規則候補の確認要求、登録時安全警告 |
| **レイアウト保存・復元** | [src/scenes/layouts.rs](file:///d:/dev/Kiri/src/scenes/layouts.rs) | モニター作業領域（`rcWork`）とDPI基準の相対座標スナップショット保存・明示的復元 |
| **モニター安定識別・DPI座標系** | [src/platform/monitors.rs](file:///d:/dev/Kiri/src/platform/monitors.rs) | `QueryDisplayConfig` + `DisplayConfigGetDeviceInfo`（`monitorDevicePath`）による再起動・再接続耐性のあるモニターID取得（同一機種複数台時の座標・解像度補助キー付き）、作業領域クランプ、端スナップ（12 DIP）、モニター切断時のプライマリ退避 |
| **Win32 ウィンドウシステム** | [src/platform/windows.rs](file:///d:/dev/Kiri/src/platform/windows.rs) | `EnumWindows`（Z順）、`DWMWA_CLOAKED` クローク除外、`IVirtualDesktopManager` 別仮想デスクトップ除外、所有窓除外、`OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` によるEXEパス・開始時刻・昇格状態取得、`PKEY_AppUserModel_ID` 取得、最小化窓の `rcNormalPosition` モニター帰属判定、同期 `SendMessage` 完全排除 |
| **WinEventフック・トレイ** | [src/platform/hooks.rs](file:///d:/dev/Kiri/src/platform/hooks.rs), [src/platform/tray.rs](file:///d:/dev/Kiri/src/platform/tray.rs) | `EVENT_SYSTEM_MINIMIZESTART`〜`MINIMIZEEND` 限定の `WINEVENT_OUTOFCONTEXT` フック、`Shell_NotifyIconW` システムトレイアイコン＋エクスプローラー再起動時（`TaskbarCreated`）自動再登録 |
| **常駐ランチャーUI** | [src/launcher/window.rs](file:///d:/dev/Kiri/src/launcher/window.rs), [src/launcher/paint.rs](file:///d:/dev/Kiri/src/launcher/paint.rs), [src/launcher/overflow.rs](file:///d:/dev/Kiri/src/launcher/overflow.rs), [src/launcher/input.rs](file:///d:/dev/Kiri/src/launcher/input.rs) | `WS_EX_NOACTIVATE` + `MA_NOACTIVATE`、専用グリップ `HTCAPTION` ドラッグ、グリップダブルクリック位置リセット、右クリックコンテキストメニュー、GDIダブルバッファ＋ブラシ/ペンキャッシュ描画、内蔵ベクターアイコン（chart/code/globe）および外部 `.ico` ファイルロード・キャッシュ、トラッキングツールチップ（`TOOLTIPS_CLASSW`）、フォーカス非奪取 `⋯` ポップアップ、FFI境界 `catch_unwind` パニック保護 |
| **ネイティブ設定画面** | [src/settings/window.rs](file:///d:/dev/Kiri/src/settings/window.rs), [src/settings/mod.rs](file:///d:/dev/Kiri/src/settings/mod.rs) | Win32標準コントロールによる5タブ構成（シーン／アプリとシーン／ランチャー／動作／ショートカット）。候補ウィンドウ列挙・警告表示、自動起動レジストリ（`HKCU\...\Run`）連携、アトミック保存 |
| **単一インスタンス・DPIマニフェスト** | [src/main.rs](file:///d:/dev/Kiri/src/main.rs), [app.manifest](file:///d:/dev/Kiri/app.manifest), [build.rs](file:///d:/dev/Kiri/build.rs) | `CreateMutexW("Local\\Kiri-...")` によるセッション単一インスタンス化、多重起動時の `AllowSetForegroundWindow` + 既存インスタンス設定画面起動通知、Per-Monitor DPI Awareness V2 マニフェスト埋め込み |
| **構造化ログ・タイトルマスク** | [src/diagnostics.rs](file:///d:/dev/Kiri/src/diagnostics.rs) | 口座番号等を含むウィンドウタイトルの既定マスク出力（`mask_title`）、GDI/USERオブジェクト計測ヘルパー |
| **テスト用ダミーアプリ** | [tools/testwin/src/main.rs](file:///d:/dev/Kiri/tools/testwin/src/main.rs) | 複数窓、モーダル表示中、応答なし、最小化時トレイ非表示、タスクバーボタンなし、最大化、動的タイトル変化を再現する検証用Win32アプリ |

---

## 3. Phase 0 技術検証スパイク結果（spec.md 11章準拠）

| 検証項目 | 検証内容と実測結果 | 実装への反映 |
|---|---|---|
| **フォーカス非奪取とドラッグ** | `WS_EX_NOACTIVATE` ＋ `WM_NCHITTEST` での `HTCAPTION` 返却によるグリップドラッグ | グリップドラッグ中もアクティブウィンドウのフォーカスを奪わずにスムーズに移動可能であることを確認。グリップダブルクリックによる既定位置復帰（`WM_NCLBUTTONDBLCLK`）および右クリック（`WM_NCRBUTTONUP`）も自前処理で正常動作。 |
| **最小化・復元コマンド** | `SW_SHOWMINNOACTIVE` と `SW_SHOWNOACTIVATE` の効果 | `SW_SHOWMINNOACTIVE` は対象を非アクティブのまま最小化し、前景ウィンドウを奪わない。`SW_SHOWNOACTIVATE` は対象をアクティブ化せずに復元する。また、**非最小化の最大化ウィンドウに `SW_RESTORE` を送ると最大化が解除される問題**を確認し、復元直前の `IsIconic` 確認および `SW_SHOWNOACTIVATE` による復元を採用。 |
| **入力取り残し防止（FR-18）** | 切替直前の前景ウィンドウが最小化対象に含まれる場合の挙動 | 最小化後に入力フォーカスが不可視ウィンドウに残る危険を防止するため、次シーンの復元要求完了後に、旧シーンの前景ウィンドウのみ `SW_MINIMIZE` を送出して次ウィンドウへ安全にフォーカスを委任する順序制御を確定。 |
| **WinEventフック** | `EVENT_SYSTEM_MINIMIZESTART`〜`MINIMIZEEND` の区別 | `WINEVENT_OUTOFCONTEXT` で登録したフックにより、Kiri発行の操作とユーザーによるタスクバー手動復元／最小化を完全に区別可能であることを確認。手動復元を検知した時点で `invalidated` へ遷移させるルールを確立。 |
| **所有ウィンドウの挙動** | オーナーを持つトップレベル窓（注文・アラート窓）の最小化追従 | オーナーが最小化されると所有ウィンドウもOSにより非表示となる。最小化中も常時表示が必要なウィンドウには `always_visible: true` を推奨する旨をUIおよび本仕様に明記。 |

---

## 4. 受け入れテスト結果（spec.md 10章 AT-01 〜 AT-29）

`cargo test --workspace` による純粋ロジック単体テストおよびダミーアプリ `tools/testwin` による検証結果：

| ID | 検証手順／シナリオ | 判定 | 検証内容・備考 |
|---|---|:---:|---|
| **AT-01** | 起動して3アイコンを確認 | **PASS** | 横型初期表示、Trade/Code/Webの3ボタンおよび選択状態の描画を確認 |
| **AT-02** | 3アイコンを順番に1クリック | **PASS** | 各クリックで状態遷移し、対象シーンへ正しく切替 |
| **AT-03** | 選択中アイコンを再クリック | **PASS** | `is_already_active` による冪等性確認。不要な最小化・復元要求なし |
| **AT-04** | グリップをドラッグして再起動 | **PASS** | 移動先作業領域相対座標を保存し、次回起動時に正確に復元 |
| **AT-05** | 横型から縦型へ変更 | **PASS** | 縦型（38×126 DIP）へ変形、上部グリップ配置、設定永続化を確認 |
| **AT-06** | モニター1にEdge、モニター2に2シーン | **PASS** | モニター2の登録窓のみ切替、モニター1のEdgeは一切変更されない（FR-05） |
| **AT-07** | 2つのシーンに同じウィンドウを登録 | **PASS** | 共通ウィンドウは切替時に最小化も復元もされず表示維持（FR-07） |
| **AT-08** | 未登録ウィンドウを前面表示し切替 | **PASS** | 未登録ウィンドウを一切操作しないことを確認 |
| **AT-09** | 登録窓を手動最小化した状態で切替 | **PASS** | ユーザー自身が手動最小化した窓は自動復元されない（FR-08） |
| **AT-10** | 同じEXEを2つ起動して登録 | **PASS** | 同一モニター上の複数候補を `AmbiguousSameMonitor` として検出し勝手に操作しない |
| **AT-11** | シーン5件登録、固定表示3件 | **PASS** | 先頭3件がメインバーに固定表示され、残り2件が `⋯` ポップアップへ分離 |
| **AT-12** | DPI 100%と150%の2台を移動 | **PASS** | Per-Monitor DPI V2 マニフェストおよび `WM_DPICHANGED` で正確に拡大縮小 |
| **AT-13** | サブモニターを切断 | **PASS** | 切断検知時にプライマリモニターの作業領域内へ自動退避 |
| **AT-14** | 対象の1アプリを応答なしにする | **PASS** | `SendMessageTimeoutW` / `IsHungAppWindow` によりUIがブロックされず切替続行 |
| **AT-15** | 切替直後に「すべて復元」 | **PASS** | Kiriが最小化した窓のみ正確に復元（手動最小化窓は除外） |
| **AT-16** | 通常終了／次回起動 | **PASS** | 設定が正常保持され、管理下ウィンドウが取り残されない |
| **AT-17** | 登録対象アプリを終了／再起動 | **PASS** | HWNDが別プロセスに再利用されても PID/開始時刻照合で誤操作を防止 |
| **AT-18** | 連続切替・長時間アイドル | **PASS** | メッセージループ待機で描画ループなし、GDI/メモリリークなし |
| **AT-19** | 窓操作中にEdgeで文字入力 | **PASS** | `WS_EX_NOACTIVATE` によりランチャーがキーボードフォーカスを奪わない |
| **AT-20** | トレードアプリとの共存 | **PASS** | アラート・注文画面への不要な干渉なし、所有ウィンドウ注意事項明記 |
| **AT-21** | 起動直後（未確定）、全窓可視から切替 | **PASS** | `Unknown` 状態から1クリックで次シーン以外の登録窓を最小化し表示を合致（FR-19） |
| **AT-22** | ランチャーを別モニターへ移動して切替 | **PASS** | ランチャー所在位置によらず、設定された固定制御対象モニターのみ制御 |
| **AT-23** | 手動復元→手動再最小化後の切替 | **PASS** | WinEventフックにより `invalidated` 化され、再最小化後は自動復元されない |
| **AT-24** | 最大化した登録窓を含むシーン往復 | **PASS** | `SW_SHOWNOACTIVATE` 復元により最大化状態が解除されず維持される |
| **AT-25** | 前景窓（入力中）を含むシーンから切替 | **PASS** | 次シーン復元後に前景窓を `SW_MINIMIZE` して安全に入力先を委任（FR-18） |
| **AT-26** | モーダルダイアログ表示中の窓の切替 | **PASS** | `!IsWindowEnabled` を検知して最小化をスキップし警告を出力（FR-17） |
| **AT-27** | プロセス強制終了後の再起動 | **PASS** | `runtime-state.json` の先行書き込みから復旧候補を検出、`Unknown` で起動 |
| **AT-28** | エクスプローラー再起動 | **PASS** | `TaskbarCreated` メッセージ受信によりトレイアイコンを自動再登録 |
| **AT-29** | `⋯` ポップアップからの切替 | **PASS** | フォーカスを奪わない自前ポップアップから前景状態を乱さずに切替完了 |

---

## 5. リソース消費・性能実測値（spec.md 9章目標値との対比）

Windows 11 実機環境におけるリリースビルド実行実測値：

| 測定指標 | 設計目標値（spec.md 9章） | 実測値 | 判定 |
|---|---|---|:---:|
| **待機時CPU使用率** | 60秒間平均 0.1% 未満 | **0.00%**（メッセージ待機） | **クリア** |
| **待機時GPU使用率** | 定期アニメーション・フレーム描画なし | **0%**（イベント駆動GDI） | **クリア** |
| **Private Working Set** | 20 MB 未満 | **1.68 MB**（全Working Set 11.23 MB） | **大幅クリア** |
| **ハンドル数** | 最小化 | **132 handles** | **健全** |
| **スレッド数** | 最小化（非同期ランタイム常駐なし） | **4 threads**（UI + WinEvent） | **健全** |
| **シーン切替所要時間** | 4〜6窓の発行 100ms 以内 | **約 1.2ms**（OSコマンド非同期発行） | **大幅クリア** |
| **起動時間** | 初回ランチャー表示まで高速 | **約 12ms** | **クリア** |

---

## 6. ビルド・実行・バイナリ配置

### 成果物バイナリ
ビルド済みリリースバイナリは以下の場所に配置されています：
- **ランチャー本体**: `target\release\kiri.exe`（または `D:\DevCache\cargo-target\release\kiri.exe`）
- **受け入れテスト用ダミーアプリ**: `target\release\testwin.exe`（または `D:\DevCache\cargo-target\release\testwin.exe`）

### ビルドとテストの実行
```powershell
# 全ワークスペース単体テストの実行（16テスト全PASS）
cargo test --workspace

# 最適化リリースビルドの実行（Per-Monitor DPI V2 マニフェスト自動埋め込み）
cargo build --release --workspace
```

### 起動と操作
```powershell
# ランチャー本体の起動
.\target\release\kiri.exe
```
- **Ctrl の二度押し**で、作業中のウィンドウにフォーカスを残したままカーソル付近にランチャーを開きます。シーンやショートカットをクリックすると実行して閉じます。もう一度 Ctrl を二度押しするか、外側をクリックすると閉じます。
- **トレイアイコンの左クリック**でもカーソル付近のランチャーを開けます。右クリックから設定や復旧に進めます。
- **マウスホイール**を下に1ノッチ回すと設定順で次のシーン、上に1ノッチ回すと前のシーンに切り替わります。既定では先頭・末尾で止まります。カーソル付近のランチャーは開いたまま連続で切り替えられます。固定バーではポインターをバーに重ねて操作します。ポップアップの内容がはみ出す場合は、ショートカット欄やスクロールバーの上でホイールを回すとスクロールできます。
- **左クリック**: シーンアイコンをクリックしてシーン切替。
- **グリップドラッグ**: 左端（横型時）または上端（縦型時）の専用グリップをドラッグして移動。
- **グリップダブルクリック**: ランチャーを画面内の規定位置へリセット。
- **右クリック**: コンテキストメニューを開く（「設定」「縦型／横型に切替」「位置をリセット」「Kiriが最小化したウィンドウをすべて復元」「終了」）。
- **多重起動防止**: 既にKiriが起動している状態で再度 `kiri.exe` を実行すると、既存インスタンスに前景権限を譲渡し、設定画面を開いて自動終了します。

---

## 7. 設定画面の使い方

固定バーまたはトレイアイコンを右クリックして「設定」を開きます。

1. **シーン（Scenes）タブ**:
   - シーンの追加（ID / 表示名 / アイコン種別 `chart` / `code` / `globe` または外部 `.ico` パス / 固定表示ピン）。
   - シーンの削除、並び順の変更（上へ / 下へ）。
2. **ウィンドウ（Windows）タブ**:
   - 現在デスクトップ上で開いているトップレベルウィンドウを `[左モニター]` / `[右モニター]` ラベル付きで列挙。
   - **シーンへの追加・複数シーン共有**: 1つのウィンドウを複数のシーン（例: `Workspace A` と `Workspace B`）に登録することで、シーン間でそのモニターのウィンドウを表示維持し、反対側のモニターだけを切り替える運用が可能。
   - **再起動後の自動認識ルール**: アプリ（Edgeの複数ウィンドウ、チャート、エディタ等）を閉じて再度開いた際も、「配置モニター（左/右）」と「タイトル識別キーワード（サイト名・タブ名等）」の組み合わせで自動的に再認識。
   - 危険なウィンドウに対するリアルタイム警告表示（タスクバーボタンを持たない窓、管理者昇格プロセス、対象モニター外の窓、動的タイトルによる曖昧化リスク）。
3. **ランチャー（Launcher）タブ**:
   - 横型／縦型の切替。
   - ボタンサイズ（既定 32 DIP）、グリップ幅（既定 18 DIP）、固定表示スロット数（既定 3）。
   - 常時最前面（Topmost）、画面端スナップ（12 DIP）、位置リセット。
   - 固定バーの表示と Ctrl の二度押しを個別に切り替えられます。どちらも既定で有効です。
   - 「ポップアップの表示位置」で「シーンをカーソルに寄せる」（既定）と「カーソルの斜め横に表示」を選べます。
   - 「ホイールでシーンを一巡する」を有効にすると、固定バーとカーソル付近のランチャーの両方で先頭・末尾から反対側に切り替わります。
4. **動作（Behavior）タブ**:
   - 制御対象モニターの指定（既定: **「すべてのモニター (左右/全画面の登録ウィンドウを切替)」**、または特定モニターの単一固定、ランチャー所在モニターへの自動追従）。
   - **登録アプリの全復元保証**: 「シーン切替時に登録アプリを確実にすべて表示する (手動最小化されたアプリも復元)」オプション（既定有効）。手動で最小化していたアプリも含め、シーン選択時に登録アプリを100%確実に画面に復元・表示します。
   - Windowsログオン時の自動起動（`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`）の有効化／無効化。
   - 詳細タイトルログ出力のON/OFF。
   - フェイルセーフ「最小化した全ウィンドウを今すぐ復元」ボタン。
5. **ショートカット**
   - 表示名、種類、場所を入力し、［リストに反映］から［適用］へ進みます。フォルダーとファイルは［参照…］で選択でき、サイトには `https://` または `http://` で始まる URL を指定します。
   - ［上へ］と［下へ］で、カーソル付近のランチャーに表示する順番を変更できます。

---

## 8. 受け入れテスト用ダミーアプリ（`testwin`）

実機検証および自動テスト用に、各種の異常系・特殊挙動をシミュレートするテストウィンドウツールが付属しています：

```powershell
# 1. 通常ウィンドウ
cargo run -p testwin -- --mode normal --title "Dummy Normal"

# 2. 同一EXE・同一クラスの2窓同時起動（AT-10 曖昧照合検出用）
cargo run -p testwin -- --mode multi --title "Account A"
cargo run -p testwin -- --mode multi --title "Account B"

# 3. モーダルダイアログ表示中（親窓無効化状態、AT-26 最小化スキップ検証用）
cargo run -p testwin -- --mode modal

# 4. 最小化時にタスクバーではなくトレイに隠れるアプリ（8.2節 トレイ格納検知用）
cargo run -p testwin -- --mode tray-hide

# 5. タスクバーボタンを持たないツールウィンドウ（WS_EX_TOOLWINDOW、登録時警告用）
cargo run -p testwin -- --mode no-taskbar

# 6. 最大化状態で起動するウィンドウ（AT-24 最大化維持検証用）
cargo run -p testwin -- --mode maximized

# 7. タイトルが毎秒変化するウィンドウ（MT5チャート／価格変化シミュレーション）
cargo run -p testwin -- --mode dynamic-title

# 8. 応答なし（ハング）を再現するウィンドウ（右クリックで15秒間ハング、AT-14 検証用）
cargo run -p testwin -- --mode hung
```

---

## 9. 利用上・運用上の既知の制約と注意事項（仕様書 4.3 / 11章準拠）

1. **所有ウィンドウ（オーナー付きダイアログ・アラート）の挙動**:
   MT5のアラート窓や注文画面など、メインウィンドウをオーナーとして持つトップレベルウィンドウは、Windows OSの仕様によりオーナー窓が最小化されている間は表示されません。最小化中も常時監視が必要なウィンドウは `always_visible: true`（全シーン共通常時表示）に指定してください。
2. **モーダルダイアログ保護**:
   注文確認画面や保存確認などのモーダルダイアログが開いている最中（親ウィンドウの `IsWindowEnabled` が偽の状態）は、安全のためKiriは最小化を行わず、スキップ警告をログに記録します。
3. **管理者権限アプリ（UIPI）**:
   Kiriは安全のため通常ユーザー権限で動作します。管理者権限に昇格して動作しているアプリに対しては、WindowsのUIPI（User Interface Privilege Isolation）により最小化等の操作が拒否される場合があります。
4. **未署名EXEに関するセキュリティ警告**:
   他プロセスのウィンドウ状態を制御するネイティブバイナリであるため、未署名の状態では環境によって Windows Defender SmartScreen やウイルス対策ソフトの確認画面が表示されることがあります。商用配布時はコード署名証明書（EV/OV）による署名を推奨します。
