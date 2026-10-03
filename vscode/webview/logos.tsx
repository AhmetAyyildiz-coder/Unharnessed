// The providers' own marks, monochrome (`currentColor`, 24 grid), from
// @lobehub/icons-static-svg (MIT). Logos are not redrawn: the hand-drawn
// rule (icons.tsx) is for our icons.
import anthropic from "@lobehub/icons-static-svg/icons/anthropic.svg";
import azure from "@lobehub/icons-static-svg/icons/azure.svg";
import bedrock from "@lobehub/icons-static-svg/icons/bedrock.svg";
import cerebras from "@lobehub/icons-static-svg/icons/cerebras.svg";
import cloudflare from "@lobehub/icons-static-svg/icons/cloudflare.svg";
import deepseek from "@lobehub/icons-static-svg/icons/deepseek.svg";
import fireworks from "@lobehub/icons-static-svg/icons/fireworks.svg";
import gemini from "@lobehub/icons-static-svg/icons/gemini.svg";
import githubcopilot from "@lobehub/icons-static-svg/icons/githubcopilot.svg";
import groq from "@lobehub/icons-static-svg/icons/groq.svg";
import huggingface from "@lobehub/icons-static-svg/icons/huggingface.svg";
import kimi from "@lobehub/icons-static-svg/icons/kimi.svg";
import mcp from "@lobehub/icons-static-svg/icons/mcp.svg";
import minimax from "@lobehub/icons-static-svg/icons/minimax.svg";
import mistral from "@lobehub/icons-static-svg/icons/mistral.svg";
import moonshot from "@lobehub/icons-static-svg/icons/moonshot.svg";
import notion from "@lobehub/icons-static-svg/icons/notion.svg";
import openai from "@lobehub/icons-static-svg/icons/openai.svg";
import opencode from "@lobehub/icons-static-svg/icons/opencode.svg";
import openrouter from "@lobehub/icons-static-svg/icons/openrouter.svg";
import vercel from "@lobehub/icons-static-svg/icons/vercel.svg";
import vertexai from "@lobehub/icons-static-svg/icons/vertexai.svg";
import workersai from "@lobehub/icons-static-svg/icons/workersai.svg";
import xai from "@lobehub/icons-static-svg/icons/xai.svg";
import xiaomimimo from "@lobehub/icons-static-svg/icons/xiaomimimo.svg";
import zai from "@lobehub/icons-static-svg/icons/zai.svg";

/** By the agent's provider id. Prime Inference has no mark in the set. */
const LOGOS: Record<string, string> = {
  anthropic,
  "amazon-bedrock": bedrock,
  "azure-openai-responses": azure,
  cerebras,
  "cloudflare-ai-gateway": cloudflare,
  "cloudflare-workers-ai": workersai,
  deepseek,
  fireworks,
  google: gemini,
  "google-vertex": vertexai,
  "github-copilot": githubcopilot,
  groq,
  huggingface,
  "kimi-coding": kimi,
  minimax,
  "minimax-cn": minimax,
  mistral,
  moonshotai: moonshot,
  "moonshotai-cn": moonshot,
  openai,
  "openai-codex": openai,
  opencode,
  "opencode-go": opencode,
  openrouter,
  "vercel-ai-gateway": vercel,
  xai,
  xiaomi: xiaomimimo,
  "xiaomi-token-plan-ams": xiaomimimo,
  "xiaomi-token-plan-cn": xiaomimimo,
  "xiaomi-token-plan-sgp": xiaomimimo,
  zai,
};

/** The MCP services the agent knows, by id; Linear has no mark in the set. */
const SERVICES: Record<string, string> = { notion };

/** The set's `<title>` would add a tooltip of its own. */
const markup = (svg: string) => svg.replace(/<title>[^<]*<\/title>/, "");

/** The provider's mark, or the first letter of its name. */
export function ProviderLogo({ provider, name }: { provider: string; name: string }) {
  const svg = LOGOS[provider];
  if (!svg) {
    return (
      <span className="logo is-letter" aria-hidden="true">
        {(name || provider).charAt(0).toUpperCase()}
      </span>
    );
  }
  return <span className="logo" aria-hidden="true" dangerouslySetInnerHTML={{ __html: markup(svg) }} />;
}

/** An MCP service the agent knows, by its id: its mark, or the first letter of its name. */
export function ServiceLogo({ service, name }: { service: string; name: string }) {
  const svg = SERVICES[service];
  if (!svg) return <ProviderLogo provider="" name={name} />;
  return <span className="logo" aria-hidden="true" dangerouslySetInnerHTML={{ __html: markup(svg) }} />;
}

/** A server you added: the MCP mark. */
export function McpLogo() {
  return <span className="logo" aria-hidden="true" dangerouslySetInnerHTML={{ __html: markup(mcp) }} />;
}
