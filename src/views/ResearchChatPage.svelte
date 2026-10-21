<script lang="ts">
  // 研究对话 — the first-class home for the agent loop.
  //
  // It used to be a toggle inside 科研: a button you had to find, opening a
  // 360px card under a page of paper rows. A capability that only exists behind
  // a toggle is, in practice, a capability nobody uses. This page is just the
  // same thread at full size.
  import ResearchChat from "../components/ResearchChat.svelte";
</script>

<div class="rcp">
  <ResearchChat mode="page" open={true} />
</div>

<style>
  .rcp {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
  }
</style>
