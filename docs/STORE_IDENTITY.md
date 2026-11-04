# Microsoft Store identity

zWorkTracker 2.0, the native Slint app, is the **first version published on the Store**. The owner decided on 2026-11-04 that no Tauri build is submitted. It uses the app reserved in Partner Center, not a new listing. Use these values in the 2.0 package manifest (`AppxManifest.xml` / packaging script) exactly as written. If any value differs, Partner Center rejects the upload or creates a separate app.

All values here are public: they appear in the package manifest of every installed copy and in the Store page URL. No secrets, certificates, passwords or keys belong in this file.

## Package identity (must match exactly)

| Manifest field | Value |
|---|---|
| `Package/Identity/Name` | `PedroMiguelRodriguesJorge.zWorkTracker` |
| `Package/Identity/Publisher` | `CN=1DEF8D7B-1D68-4604-8047-048DC92CF3A9` |
| `Package/Properties/PublisherDisplayName` | `Pedro Miguel Rodrigues Jorge` |
| `Package/Properties/DisplayName` | `zWorkTracker` |
| `Package/Identity/ProcessorArchitecture` | `x64` |
| `Package/Identity/Version` | `2.0.0.0` for the first submission. Each later submission must be higher, and the last part stays `0` for Store packages. |

The Package Family Name follows from Name and Publisher: `PedroMiguelRodriguesJorge.zWorkTracker_54k2cbaqgvhqp`. A packaging test should compute it and fail if it changes.

## Store listing

| Item | Value |
|---|---|
| Store ID | `9P9RM9G0QBRC` |
| Store page | https://apps.microsoft.com/detail/9P9RM9G0QBRC |
| Store protocol link | `ms-windows-store://pdp/?productid=9P9RM9G0QBRC` |
| Website button | `<a class="btn" href="https://apps.microsoft.com/detail/9P9RM9G0QBRC">Get it from Microsoft Store</a>` |
| Copyright | Copyright (c) 2026 Pedro Miguel Rodrigues Jorge |
| Developed by | Pedro Miguel Rodrigues Jorge |
| Support contact | pmrj@rustcircle.com |
| Category | Productivity |
| MSA app ID | `561b212b-0a7a-4a47-a49d-94c7b6d51893`: not used, because the app signs in to no Microsoft account |

## Rules carried over from the Tauri app

- The Store package is **unsigned**: the Store signs it. Only local test packages are signed, with a separate **test identity** whose self-signed certificate subject matches its own Publisher.
- Test signing keys stay outside the repo, never committed.
- Before submitting, run the Windows App Certification Kit (WACK) against a copy of the Store package signed with a short-lived certificate whose subject is the Store Publisher above.
- Ship `resources.pri` with the unplated `Square44x44Logo.targetsize-*_altform-unplated.png` icons, or Windows shows the taskbar icon on a plate.
- Capability: `runFullTrust` (full-trust desktop app). No network capability is needed, because the app makes no network calls.

## Reference in the Tauri repo

- `STORE_LISTING_NOTICE.md`: listing text, features, notices, privacy policy draft and certification notes
- `scripts/package-msix.mjs`, `scripts/make-pri.mjs`, `scripts/msix-vm.mjs`: packaging, the icon index (PRI), and the WACK and VM-install flow
- `packaging/msix/{store,test}/resources.pri`: created by the first real `pnpm msix:vm` run on 1.4.0; not there yet
