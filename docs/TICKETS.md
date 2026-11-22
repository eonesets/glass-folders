Кратко: тикеты v0.3 — плитки растягиваются шагами по сетке, убрать полоску сверху в окне папки, перетаскивание ярлыков между группами и в новую группу.
Исполнители: T1+T2 — агент Sonnet, T3 — агент Opus (по очереди, одни и те же файлы).

# Tickets v0.3

## T1. Tile resize snaps to grid — agent: Sonnet/medium — status: done
User: tile on desktop must not take any size. Resize in fixed steps: +1 icon column right, +1 row down.
- Tile size = N cols x M rows of icon cells (+ padding / name strip). Resize grip snaps to whole cells (round to nearest while dragging).
- Move (drag) also snaps to a fixed desktop grid (same cell pitch), so tiles align.
- Min 2x2 cells (as now). Saved "tile" rect in .glassfolder.json stays valid; snap on load.
- Done: dragging grip changes size only by whole cells; icons fill grid exactly, no partial gaps.
- Result: plat.rs snap_size/snap_pos (cell 48x42 logical, w=cols*48+12, h=rows*42+30, origin = work area corner); used in drag, on load, default rect. app.slint cols/rows use same numbers.

## T2. No strip at top of opened folder panel — agent: Sonnet/medium — status: done
User: when folder opened, there is some strip/line at top. Must not exist.
- Find source: likely DWM non-client frame / top border (acrylic window, caption color), or header Rectangle in app.slint Panel.
- Remove it at root (DWMWA_BORDER_COLOR none / proper frame extend / layout), keep rounded corners + acrylic + drag by header + X + gear.
- Done: screenshot of open panel shows no strip on top edge.
- Result: cause = DWM active-caption band over extended frame (panel is really active). win::glass now sets DWMWA_CAPTION_COLOR none for panel too (was tile-only).

## T3. Drag shortcuts inside panel — agent: Opus/high — status: done
Groups = real subfolders, "Разное" = root files of folder (see docs/SPEC.md, src/model.rs).
- Press-and-drag a slot -> ghost icon follows cursor; drop on another group -> file moved (fs rename) into that subfolder (root for "Разное"); layout rebuilds.
- Drop on empty panel area (outside groups) -> create new subfolder ("Новая группа", unique name), move file there, then inline rename field for group name (Enter = rename folder, Esc = keep default).
- Short click without move still launches (threshold ~5px).
- Name collisions: append " (2)". Errors -> no crash, item stays.
- Done: move between groups works, new group created + renamed, files really moved on disk.
- Result: Slot press+move >5px = drag (ghost, target group highlight, "+ Новая группа" zone at bottom while dragging); drop -> MoveFileExW no-replace + " (n)" names; emptied subfolder removed only if truly empty; new group -> inline TextInput (Enter rename, Esc/focus loss keep); double-click group title renames too; cfg keys follow rename. plat.rs now forwards WM_CHAR + edit keys. Verified on Desktop\GF_TEST.
