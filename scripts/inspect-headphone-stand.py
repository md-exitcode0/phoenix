"""Independent read-only Blender acceptance probe; run after opening the blend.

Prints evaluated geometry, not the model's self-reported verification JSON.
This is specific to the dimensioned headphone-stand acceptance brief.
"""
import json
import bpy
from mathutils import Vector
from bpy_extras.object_utils import world_to_camera_view

graph = bpy.context.evaluated_depsgraph_get()
objects = {
    obj.name: obj.evaluated_get(graph)
    for obj in bpy.context.scene.objects
    if obj.type == 'MESH' and not obj.name.startswith('Studio_')
}
bounds = {}
for name, obj in objects.items():
    points = [obj.matrix_world @ vertex.co for vertex in obj.data.vertices]
    if not points:
        continue
    low = [min(point[axis] for point in points) * 1000 for axis in range(3)]
    high = [max(point[axis] for point in points) * 1000 for axis in range(3)]
    bounds[name] = {'min_mm': low, 'max_mm': high,
                    'size_mm': [high[i] - low[i] for i in range(3)]}


def surface_z(name, x, y, from_above=True):
    obj = objects[name]
    inverse = obj.matrix_world.inverted()
    origin = inverse @ Vector((x, y, 1 if from_above else -1))
    direction = inverse.to_3x3() @ Vector((0, 0, -1 if from_above else 1))
    hit, position, _, _ = obj.ray_cast(origin, direction.normalized())
    return (obj.matrix_world @ position).z * 1000 if hit else None


height = max(b['max_mm'][2] for b in bounds.values()) - min(b['min_mm'][2] for b in bounds.values())
# Resolve component roles, not the first builder's exact object names. The
# brief requires a 75 mm saddle, not 75 mm cork padding or a fixed channel Y.
base_name = max((n for n in bounds if n.lower().startswith('base_')),
                key=lambda n: bounds[n]['size_mm'][0] * bounds[n]['size_mm'][1])
cores = [n for n in bounds if 'saddle' in n.lower() and ('core' in n.lower() or 'graphite' in n.lower())]
pads = [n for n in bounds if 'saddle' in n.lower() and ('cork' in n.lower() or 'pad' in n.lower())]
channels = [n for n in bounds if 'cable_channel' in n.lower()]
if len(cores) != 1 or len(pads) != 1 or len(channels) != 1:
    raise ValueError('Ambiguous component mapping; requires independent inspection, not a guessed pass')
core_name, pad_name, channel_name = cores[0], pads[0], channels[0]
channel_x, channel_y = [(bounds[channel_name]['min_mm'][a] + bounds[channel_name]['max_mm'][a]) / 2000 for a in (0, 1)]
pad_y = (bounds[pad_name]['min_mm'][1] + bounds[pad_name]['max_mm'][1]) / 2000
gap_samples = []
for x in (-0.025, 0, 0.025):
    core = surface_z(core_name, x, pad_y)
    pad = surface_z(pad_name, x, pad_y, False)
    gap_samples.append(None if core is None or pad is None else pad - core)
channel = surface_z(base_name, channel_x, channel_y)
flat = surface_z(base_name, 0.030, channel_y)
camera_points = [world_to_camera_view(bpy.context.scene, bpy.context.scene.camera, obj.matrix_world @ vertex.co)
                 for obj in objects.values() for vertex in obj.data.vertices]
frame_bounds = {'min_x': min(p.x for p in camera_points), 'max_x': max(p.x for p in camera_points),
                'min_y': min(p.y for p in camera_points), 'max_y': max(p.y for p in camera_points)}
checks = {
    'base_dimensions': all(abs(actual - wanted) < 0.1 for actual, wanted in zip(bounds[base_name]['size_mm'], (140, 110, 12))),
    'overall_height_240mm': abs(height - 240) < 0.1,
    'saddle_width_75mm': abs(bounds[core_name]['size_mm'][0] - 75) < 0.1,
    # A small compression/interference fit is a connection, not floating.
    'pad_contacts_core_at_sample_points': all(gap is not None and -0.5 <= gap <= 0.1 for gap in gap_samples),
    'channel_is_recessed': channel is not None and flat is not None and flat - channel > 1,
    'product_fully_in_frame': all(p.z > 0 and 0.05 - 1e-6 <= p.x <= 0.95 + 1e-6 and 0.05 - 1e-6 <= p.y <= 0.95 + 1e-6 for p in camera_points),
}
print('INDEPENDENT_ACCEPTANCE=' + json.dumps({
    'bounds': bounds, 'overall_height_mm': height,
    'component_mapping': {'base': base_name, 'core': core_name, 'pad': pad_name, 'channel': channel_name},
    'camera_frame_bounds': frame_bounds,
    'required_frame_margin': 0.05,
    'pad_core_gaps_mm': gap_samples, 'channel_base_z_mm': channel,
    'flat_base_z_mm': flat, 'checks': checks, 'passed': all(checks.values()),
    'limits': 'Contact samples do not prove full connectivity, manufacturability, or visual quality.',
}))
