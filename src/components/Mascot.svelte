<script lang="ts">
  // 极简几何 mascot：薄荷绿圆 + 线条眼 + 腮红，4 个 state，纯 SVG / CSS。
  // 用 CSS 关键帧 + transform，不引入 motion 库。
  type State = "idle" | "sleep" | "celebrate" | "curious";
  type Size = "sm" | "md" | "lg";
  let { state = "idle" as State, size = "md" as Size, title = "" }:
    { state?: State; size?: Size; title?: string } = $props();
</script>

<svg
  class="mascot mascot-{state} mascot-size-{size}"
  viewBox="0 0 100 100"
  role="img"
  aria-label={title || `mascot ${state}`}
>
  <!-- subtle floor shadow -->
  <ellipse class="m-shadow" cx="50" cy="92" rx="22" ry="3" />
  <!-- body -->
  <circle class="m-body" cx="50" cy="54" r="36" />
  <!-- cheeks -->
  <circle class="m-cheek" cx="30" cy="62" r="6" />
  <circle class="m-cheek" cx="70" cy="62" r="6" />
  <!-- eyes (slot) -->
  <g class="m-eyes">
    <g class="m-eye">
      <circle class="m-pupil" cx="40" cy="50" r="3" />
    </g>
    <g class="m-eye">
      <circle class="m-pupil" cx="60" cy="50" r="3" />
    </g>
  </g>
  <!-- mouth -->
  {#if state === "sleep"}
    <path class="m-mouth" d="M 44 70 Q 50 66 56 70" />
  {:else if state === "celebrate"}
    <path class="m-mouth" d="M 40 68 Q 50 80 60 68" />
    <!-- confetti eyes: small stars above the eye slot -->
    <g class="m-spark">
      <path d="M 40 42 l 1.5 3 l 3 0.5 l -2.5 2 l 0.6 3 l -2.6 -1.6 l -2.6 1.6 l 0.6 -3 l -2.5 -2 l 3 -0.5 z" />
      <path d="M 60 42 l 1.5 3 l 3 0.5 l -2.5 2 l 0.6 3 l -2.6 -1.6 l -2.6 1.6 l 0.6 -3 l -2.5 -2 l 3 -0.5 z" />
    </g>
  {:else}
    <path class="m-mouth" d="M 42 70 Q 50 75 58 70" />
  {/if}
  {#if state === "curious"}
    <!-- one eyebrow up -->
    <path class="m-brow" d="M 36 40 q 4 -2 8 0" />
  {/if}
</svg>

<style>
  .mascot {
    overflow: visible;
    display: block;
  }
  .mascot-size-sm { width: var(--mascot-sm); height: var(--mascot-sm); }
  .mascot-size-md { width: var(--mascot-md); height: var(--mascot-md); }
  .mascot-size-lg { width: var(--mascot-lg); height: var(--mascot-lg); }
  .m-shadow {
    fill: var(--accent);
    opacity: 0.18;
  }
  .m-body {
    fill: var(--accent);
    stroke: color-mix(in oklab, var(--accent) 60%, var(--bg) 30%);
    stroke-width: 1.5;
  }
  .m-cheek {
    fill: var(--tone-peach);
    opacity: 0.65;
  }
  .m-pupil {
    fill: var(--accent-fg);
  }
  .m-mouth {
    fill: none;
    stroke: var(--accent-fg);
    stroke-width: 2.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .m-brow {
    fill: none;
    stroke: var(--accent-fg);
    stroke-width: 2;
    stroke-linecap: round;
    opacity: 0.75;
  }
  .m-spark path {
    fill: var(--tone-amber);
  }
  /* state animations */
  .mascot-idle {
    animation: m-bob 3.2s ease-in-out infinite;
  }
  @keyframes m-bob {
    0%, 100% { transform: translateY(0); }
    50%      { transform: translateY(-2.5px); }
  }
  .mascot-sleep .m-pupil {
    transform: scaleY(0.25);
    transform-origin: center;
    transition: transform var(--dur) var(--ease);
  }
  .mascot-sleep .m-body {
    filter: saturate(0.7) brightness(0.92);
  }
  .mascot-celebrate {
    animation: m-pop 0.7s var(--ease-out-back) both;
  }
  @keyframes m-pop {
    0%   { transform: scale(0.85) rotate(-3deg); }
    60%  { transform: scale(1.1)  rotate(3deg); }
    100% { transform: scale(1)    rotate(0); }
  }
  .mascot-celebrate .m-spark {
    animation: m-spark 0.9s ease-out both;
  }
  @keyframes m-spark {
    0%   { opacity: 0; transform: translateY(0) scale(0.5); }
    60%  { opacity: 1; transform: translateY(-4px) scale(1); }
    100% { opacity: 0; transform: translateY(-10px) scale(0.7); }
  }
  .mascot-curious {
    animation: m-tilt 2.4s ease-in-out infinite;
    transform-origin: 50% 90%;
  }
  @keyframes m-tilt {
    0%, 100% { transform: rotate(-4deg); }
    50%      { transform: rotate(4deg); }
  }
</style>