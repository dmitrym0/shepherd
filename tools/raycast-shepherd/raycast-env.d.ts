/// <reference types="@raycast/api">

/* 🚧 🚧 🚧
 * This file is auto-generated from the extension's manifest.
 * Do not modify manually. Instead, update the `package.json` file.
 * 🚧 🚧 🚧 */

/* eslint-disable @typescript-eslint/ban-types */

type ExtensionPreferences = {
  /** focus-agent.sh path - Path to shepherd's focus-agent.sh */
  "focusScript": string
}

/** Preferences accessible in all the extension's commands */
declare type Preferences = ExtensionPreferences

declare namespace Preferences {
  /** Preferences accessible in the `focus-agent` command */
  export type FocusAgent = ExtensionPreferences & {}
}

declare namespace Arguments {
  /** Arguments passed to the `focus-agent` command */
  export type FocusAgent = {}
}

