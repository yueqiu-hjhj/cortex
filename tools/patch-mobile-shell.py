import re, sys
p = r'D:\cortex-base\src\App.svelte'
with open(p, 'r', encoding='utf-8') as f:
    c = f.read()
vw_decl = 'let vw = (typeof window !== "undefined" ? window.innerWidth : 1280);'
new_decl = vw_decl + (
    '\n  // Mobile/desktop branch is reactive: same WebView at 420px after a resize'
    '\n  // should switch to MobileShell, even on Windows where isMobile() is false.'
    '\n  const mobileNow = (isMobile || vw < 640);'
)
assert vw_decl in c, 'vw decl not found'
c2 = c.replace(vw_decl, new_decl, 1)
old_branch = '{:else if isMobile}'
new_branch = '{:else if mobileNow}'
assert c2.count(old_branch) >= 1
c3 = c2.replace(old_branch, new_branch, 1)
with open(p, 'w', encoding='utf-8') as f:
    f.write(c3)
print('OK len', len(c3))
