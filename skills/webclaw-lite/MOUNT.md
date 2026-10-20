# Mounting webclaw for agents

## DeepSeek Harness (recommended)

1. Build or download `webclaw-lite`
2. `npm install @webclaw/dsh-plugin`
3. Patch cordis with [examples/cordis.patch.yml](../../examples/cordis.patch.yml)
4. Add GitHub topic `dsh-plugin` on the plugin repo for discovery

Tools spawn the CLI; Skill teaches the recipe language.

## Generic shell agents

Copy this folder into the agent skills directory and grant shell access to `webclaw-lite`.
