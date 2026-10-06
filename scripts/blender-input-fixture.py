"""Observe scene inventory during native input testing; never construct geometry."""
import bpy
import bmesh
import json
import os
import sys
from pathlib import Path

destination = Path(sys.argv[sys.argv.index("--") + 1])

def observe():
    value = {"objects": [{"name": o.name, "type": o.type, "location": list(o.location),
                          "selected": o.select_get()} for o in bpy.context.scene.objects]}
    value["edit_meshes"] = [{"name": o.name, "vertices": len(bmesh.from_edit_mesh(o.data).verts),
                              "selected_vertices": sum(v.select for v in bmesh.from_edit_mesh(o.data).verts)}
                             for o in bpy.context.objects_in_mode if o.type == 'MESH' and o.mode == 'EDIT']
    value["views"] = [{"perspective": area.spaces.active.region_3d.view_perspective,
                       "overlays": area.spaces.active.overlay.show_overlays,
                       "rotation": list(area.spaces.active.region_3d.view_rotation)}
                      for area in bpy.context.screen.areas if area.type == 'VIEW_3D']
    value["overlay_shortcuts"] = [
        {"key": item.type, "shift": item.shift, "ctrl": item.ctrl, "alt": item.alt}
        for keymap in bpy.context.window_manager.keyconfigs.active.keymaps
        for item in keymap.keymap_items
        if item.idname == 'wm.context_toggle'
        and getattr(item.properties, 'data_path', '') == 'space_data.overlay.show_overlays'
    ]
    value["file_browsers"] = [
        {"filename": area.spaces.active.params.filename,
         "directory": os.fsdecode(area.spaces.active.params.directory)}
        for window in bpy.context.window_manager.windows
        for area in window.screen.areas
        if area.type == 'FILE_BROWSER' and area.spaces.active.params is not None
    ]
    temporary = destination.with_suffix(".tmp")
    temporary.write_text(json.dumps(value))
    os.replace(temporary, destination)
    return 0.1

bpy.app.timers.register(observe, first_interval=0.1, persistent=True)
