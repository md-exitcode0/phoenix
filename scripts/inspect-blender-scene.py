"""Read-only artifact evidence, after opening a saved scene with --disable-autoexec.

Does not create, repair, render, or save any scene. Geometry/material inventory
supports independent review; a banana's recognizability is a visual judgment.
"""
import json
import math
import bpy
from bpy_extras.object_utils import world_to_camera_view

scene = bpy.context.scene
graph = bpy.context.evaluated_depsgraph_get()
objects = []
for source in scene.objects:
    row = {"name": source.name, "type": source.type,
           "location": list(source.location), "rotation_euler": list(source.rotation_euler),
           "scale": list(source.scale),
           "hidden_render": source.hide_render,
           "modifiers": [{"name": m.name, "type": m.type} for m in source.modifiers]}
    if source.type in {"MESH", "CURVE", "SURFACE", "FONT", "META"}:
        evaluated = source.evaluated_get(graph)
        mesh = evaluated.to_mesh()
        try:
            points = [evaluated.matrix_world @ v.co for v in mesh.vertices]
            row.update(vertices=len(points), polygons=len(mesh.polygons),
                       smooth_polygons=sum(p.use_smooth for p in mesh.polygons))
            row["finite_vertices"] = all(math.isfinite(c) for p in points for c in p)
            if points and row["finite_vertices"]:
                low = [min(p[a] for p in points) for a in range(3)]
                high = [max(p[a] for p in points) for a in range(3)]
                row["world_bounds"] = {"min": low, "max": high,
                                       "size": [b-a for a,b in zip(low,high)]}
                if scene.camera:
                    projected = [world_to_camera_view(scene, scene.camera, p) for p in points]
                    row["camera_bounds"] = {
                        "min": [min(p[a] for p in projected) for a in range(3)],
                        "max": [max(p[a] for p in projected) for a in range(3)],
                        "vertices_outside_frame": sum(p.z <= 0 or not (0 <= p.x <= 1 and 0 <= p.y <= 1) for p in projected)}
            row["materials"] = []
            for material in mesh.materials:
                if material is None:
                    row["materials"].append(None)
                    continue
                entry = {"name": material.name, "diffuse_color": list(material.diffuse_color)}
                if material.use_nodes and material.node_tree:
                    entry["principled"] = []
                    for node in material.node_tree.nodes:
                        if node.type != "BSDF_PRINCIPLED":
                            continue
                        base = node.inputs.get("Base Color")
                        entry["principled"].append({
                            "base_color": list(base.default_value) if base else None,
                            "base_color_linked": bool(base and base.is_linked)})
                row["materials"].append(entry)
        finally:
            evaluated.to_mesh_clear()
    objects.append(row)

print("SCENE_EVIDENCE=" + json.dumps({
    "file": bpy.data.filepath, "objects": objects,
    "camera": scene.camera.name if scene.camera else None,
    "render": {"engine": scene.render.engine, "width": scene.render.resolution_x,
               "height": scene.render.resolution_y, "percentage": scene.render.resolution_percentage},
    "units": {"system": scene.unit_settings.system, "scale": scene.unit_settings.scale_length},
    "visual_acceptance": "unverified; independently inspect preview and editable geometry",
}, allow_nan=False))
