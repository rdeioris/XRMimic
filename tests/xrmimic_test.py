#!/usr/bin/env python3
"""XRMimic feature test: pure Python, standard library only (ctypes, unittest).

Drives xrmimic_runtime.dll directly as an OpenXR app would: it negotiates
with the runtime like the OpenXR loader, makes its own D3D11 device through
d3d11.dll, renders with plain clears and texture uploads, and checks what the
runtime reports through the API and what the headset window shows in its
snapshots (XRMIMIC_TEST_SNAPSHOT) and frame captures (XRMIMIC_CAPTURE).

    python tests/xrmimic_test.py            run every check (Windows, after cargo build --release)
    python tests/xrmimic_test.py -v -k layers
    python tests/xrmimic_test.py --scenario api     one scenario, its checks printed

XRMimic reads some settings once per process (device preset, hands, test
motion), so each scenario runs in a child process with its own environment.
Scenarios that check pictures open the headset window for a few seconds;
nothing is typed or clicked on the desktop. XRMIMIC_TEST_DLL overrides the
runtime's path.

Covered: the control API (XRMIMIC_CONTROL) and the frame-stepped clock
(XRMIMIC_CLOCK=step), loader negotiation, extensions, instance and system properties, view
configurations, blend modes, strings and paths, D3D11 requirements, the
session lifecycle and its events, reference spaces (VIEW, LOCAL, STAGE,
LOCAL_FLOOR) and STAGE bounds, swapchain formats and their errors, MSAA
colour and depth, frame pacing, time conversion, actions (Touch bindings,
state, poses, bound sources, names), haptics, the simulated head motion, a
controller put down apart from the head, the Input tab, hands
(XR_EXT_hand_tracking, XR_EXT_hand_interaction), device presets, a play-area
change event, and in the window: the projection layer, quad (both eyes and
one eye), cylinder, equirect and cube layers, the Depth view, the Centre
view, reprojection with and without depth, depth-tested layers, the
play-area boundary and the haptics bar. Not covered here: Chrome (see
tools/smoke/smoke.mjs), the runtime switch, the Linux/Vulkan backend.
"""

import ctypes
import json
import math
import os
import subprocess
import sys
import tempfile
import time
import unittest
from ctypes import (POINTER, Structure, Union, byref, c_char, c_char_p, c_float, c_int32, c_int64, c_long,
                    c_uint8, c_uint32, c_uint64, c_void_p, sizeof)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DLL = os.environ.get('XRMIMIC_TEST_DLL') or os.path.join(ROOT, 'target', 'release', 'xrmimic_runtime.dll')

# ---------------------------------------------------------------- OpenXR constants

XR = dict(
    SUCCESS=0, TIMEOUT_EXPIRED=1, EVENT_UNAVAILABLE=4, SPACE_BOUNDS_UNAVAILABLE=7, SESSION_NOT_FOCUSED=8,
    ERROR_VALIDATION_FAILURE=-1, ERROR_FEATURE_UNSUPPORTED=-8, ERROR_EXTENSION_NOT_PRESENT=-9,
    ERROR_SIZE_INSUFFICIENT=-11, ERROR_HANDLE_INVALID=-12, ERROR_SESSION_NOT_RUNNING=-16,
    ERROR_PATH_FORMAT_INVALID=-21, ERROR_PATH_UNSUPPORTED=-22, ERROR_SWAPCHAIN_FORMAT_UNSUPPORTED=-26,
    ERROR_ACTION_TYPE_MISMATCH=-27, ERROR_SESSION_NOT_STOPPING=-29, ERROR_REFERENCE_SPACE_UNSUPPORTED=-31,
    ERROR_CALL_ORDER_INVALID=-37, ERROR_ACTIONSET_NOT_ATTACHED=-46,
)
RESULT_NAME = {v: k for k, v in XR.items()}

T_EXTENSION_PROPERTIES = 2
T_INSTANCE_CREATE_INFO = 3
T_SYSTEM_GET_INFO = 4
T_SYSTEM_PROPERTIES = 5
T_VIEW_LOCATE_INFO = 6
T_VIEW = 7
T_SESSION_CREATE_INFO = 8
T_SWAPCHAIN_CREATE_INFO = 9
T_SESSION_BEGIN_INFO = 10
T_VIEW_STATE = 11
T_FRAME_END_INFO = 12
T_HAPTIC_VIBRATION = 13
T_EVENT_DATA_BUFFER = 16
T_EVENT_SESSION_STATE_CHANGED = 18
T_ACTION_STATE_BOOLEAN = 23
T_ACTION_STATE_FLOAT = 24
T_ACTION_STATE_VECTOR2F = 25
T_ACTION_STATE_POSE = 27
T_ACTION_SET_CREATE_INFO = 28
T_ACTION_CREATE_INFO = 29
T_INSTANCE_PROPERTIES = 32
T_COMPOSITION_LAYER_PROJECTION = 35
T_COMPOSITION_LAYER_QUAD = 36
T_REFERENCE_SPACE_CREATE_INFO = 37
T_ACTION_SPACE_CREATE_INFO = 38
T_EVENT_REFERENCE_SPACE_CHANGE_PENDING = 40
T_VIEW_CONFIGURATION_VIEW = 41
T_SPACE_LOCATION = 42
T_FRAME_STATE = 44
T_COMPOSITION_LAYER_PROJECTION_VIEW = 48
T_INTERACTION_PROFILE_SUGGESTED_BINDING = 51
T_EVENT_INTERACTION_PROFILE_CHANGED = 52
T_INTERACTION_PROFILE_STATE = 53
T_SWAPCHAIN_IMAGE_WAIT_INFO = 56
T_ACTION_STATE_GET_INFO = 58
T_HAPTIC_ACTION_INFO = 59
T_SESSION_ACTION_SETS_ATTACH_INFO = 60
T_ACTIONS_SYNC_INFO = 61
T_BOUND_SOURCES_FOR_ACTION_ENUMERATE_INFO = 62
T_INPUT_SOURCE_LOCALIZED_NAME_GET_INFO = 63
T_COMPOSITION_LAYER_CUBE_KHR = 1000006000
T_COMPOSITION_LAYER_DEPTH_INFO_KHR = 1000010000
T_COMPOSITION_LAYER_CYLINDER_KHR = 1000017000
T_GRAPHICS_BINDING_D3D11_KHR = 1000027000
T_SWAPCHAIN_IMAGE_D3D11_KHR = 1000027001
T_GRAPHICS_REQUIREMENTS_D3D11_KHR = 1000027002
T_HAND_TRACKER_CREATE_INFO_EXT = 1000051001
T_HAND_JOINTS_LOCATE_INFO_EXT = 1000051002
T_HAND_JOINT_LOCATIONS_EXT = 1000051003
T_COMPOSITION_LAYER_EQUIRECT2_KHR = 1000091000
T_COMPOSITION_LAYER_DEPTH_TEST_FB = 1000212000

SPACE_VIEW, SPACE_LOCAL, SPACE_STAGE, SPACE_LOCAL_FLOOR = 1, 2, 3, 1000426000
SPACE_UNBOUNDED_MSFT = 1000038000
STATE_NAMES = ['UNKNOWN', 'IDLE', 'READY', 'SYNCHRONIZED', 'VISIBLE', 'FOCUSED', 'STOPPING', 'LOSS_PENDING', 'EXITING']
BOOLEAN_INPUT, FLOAT_INPUT, VECTOR2F_INPUT, POSE_INPUT, VIBRATION_OUTPUT = 1, 2, 3, 4, 100
EYES_BOTH, EYES_LEFT = 0, 1
LAYER_ALPHA = 0x2
USAGE_COLOR, USAGE_DEPTH = 0x1, 0x2
LOC_ORIENTATION_VALID, LOC_POSITION_VALID = 0x1, 0x2
INFINITE_DURATION = 0x7FFFFFFFFFFFFFFF
RGBA8, RGBA8_SRGB, D32_FLOAT = 28, 29, 40

# ---------------------------------------------------------------- OpenXR structures


class Quat(Structure):
    _fields_ = [('x', c_float), ('y', c_float), ('z', c_float), ('w', c_float)]


class Vec3(Structure):
    _fields_ = [('x', c_float), ('y', c_float), ('z', c_float)]


class Posef(Structure):
    _fields_ = [('orientation', Quat), ('position', Vec3)]


def pose(x=0.0, y=0.0, z=0.0, q=(0.0, 0.0, 0.0, 1.0)):
    return Posef(Quat(*q), Vec3(x, y, z))


class Fovf(Structure):
    _fields_ = [('left', c_float), ('right', c_float), ('up', c_float), ('down', c_float)]


class Extent2Df(Structure):
    _fields_ = [('width', c_float), ('height', c_float)]


class Rect2Di(Structure):
    _fields_ = [('x', c_int32), ('y', c_int32), ('width', c_int32), ('height', c_int32)]


class XrStruct(Structure):
    """A structure with `type` and `next` first; the type is filled in."""

    def __init__(self, **kw):
        super().__init__()
        if hasattr(self, 'TYPE'):
            self.type = self.TYPE
        for k, v in kw.items():
            setattr(self, k, v)


def xr_struct(name, type_, fields):
    return type(name, (XrStruct,), {'TYPE': type_, '_fields_': [('type', c_int32), ('next', c_void_p)] + fields})


ApplicationInfo = type('ApplicationInfo', (Structure,), {'_fields_': [
    ('applicationName', c_char * 128), ('applicationVersion', c_uint32),
    ('engineName', c_char * 128), ('engineVersion', c_uint32), ('apiVersion', c_uint64)]})
InstanceCreateInfo = xr_struct('InstanceCreateInfo', T_INSTANCE_CREATE_INFO, [
    ('createFlags', c_uint64), ('applicationInfo', ApplicationInfo),
    ('enabledApiLayerCount', c_uint32), ('enabledApiLayerNames', POINTER(c_char_p)),
    ('enabledExtensionCount', c_uint32), ('enabledExtensionNames', POINTER(c_char_p))])
ExtensionProperties = xr_struct('ExtensionProperties', T_EXTENSION_PROPERTIES, [
    ('extensionName', c_char * 128), ('extensionVersion', c_uint32)])
InstanceProperties = xr_struct('InstanceProperties', T_INSTANCE_PROPERTIES, [
    ('runtimeVersion', c_uint64), ('runtimeName', c_char * 128)])
SystemGetInfo = xr_struct('SystemGetInfo', T_SYSTEM_GET_INFO, [('formFactor', c_int32)])
SystemProperties = xr_struct('SystemProperties', T_SYSTEM_PROPERTIES, [
    ('systemId', c_uint64), ('vendorId', c_uint32), ('systemName', c_char * 256),
    ('maxSwapchainImageHeight', c_uint32), ('maxSwapchainImageWidth', c_uint32), ('maxLayerCount', c_uint32),
    ('orientationTracking', c_uint32), ('positionTracking', c_uint32)])
ViewConfigurationView = xr_struct('ViewConfigurationView', T_VIEW_CONFIGURATION_VIEW, [
    ('recommendedImageRectWidth', c_uint32), ('maxImageRectWidth', c_uint32),
    ('recommendedImageRectHeight', c_uint32), ('maxImageRectHeight', c_uint32),
    ('recommendedSwapchainSampleCount', c_uint32), ('maxSwapchainSampleCount', c_uint32)])
GraphicsRequirementsD3D11 = xr_struct('GraphicsRequirementsD3D11', T_GRAPHICS_REQUIREMENTS_D3D11_KHR, [
    ('luidLow', c_uint32), ('luidHigh', c_int32), ('minFeatureLevel', c_int32)])
GraphicsBindingD3D11 = xr_struct('GraphicsBindingD3D11', T_GRAPHICS_BINDING_D3D11_KHR, [('device', c_void_p)])
SessionCreateInfo = xr_struct('SessionCreateInfo', T_SESSION_CREATE_INFO, [
    ('createFlags', c_uint64), ('systemId', c_uint64)])
SessionBeginInfo = xr_struct('SessionBeginInfo', T_SESSION_BEGIN_INFO, [('primaryViewConfigurationType', c_int32)])
EventDataBuffer = xr_struct('EventDataBuffer', T_EVENT_DATA_BUFFER, [('varying', c_uint8 * 4000)])
EventSessionStateChanged = xr_struct('EventSessionStateChanged', T_EVENT_SESSION_STATE_CHANGED, [
    ('session', c_uint64), ('state', c_int32), ('time', c_int64)])
EventReferenceSpaceChangePending = xr_struct('EventReferenceSpaceChangePending', T_EVENT_REFERENCE_SPACE_CHANGE_PENDING, [
    ('session', c_uint64), ('referenceSpaceType', c_int32), ('changeTime', c_int64), ('poseValid', c_uint32),
    ('poseInPreviousSpace', Posef)])
ReferenceSpaceCreateInfo = xr_struct('ReferenceSpaceCreateInfo', T_REFERENCE_SPACE_CREATE_INFO, [
    ('referenceSpaceType', c_int32), ('poseInReferenceSpace', Posef)])
SpaceLocation = xr_struct('SpaceLocation', T_SPACE_LOCATION, [('locationFlags', c_uint64), ('pose', Posef)])
SwapchainCreateInfo = xr_struct('SwapchainCreateInfo', T_SWAPCHAIN_CREATE_INFO, [
    ('createFlags', c_uint64), ('usageFlags', c_uint64), ('format', c_int64), ('sampleCount', c_uint32),
    ('width', c_uint32), ('height', c_uint32), ('faceCount', c_uint32), ('arraySize', c_uint32),
    ('mipCount', c_uint32)])
SwapchainImageD3D11 = xr_struct('SwapchainImageD3D11', T_SWAPCHAIN_IMAGE_D3D11_KHR, [('texture', c_void_p)])
SwapchainImageWaitInfo = xr_struct('SwapchainImageWaitInfo', T_SWAPCHAIN_IMAGE_WAIT_INFO, [('timeout', c_int64)])
FrameState = xr_struct('FrameState', T_FRAME_STATE, [
    ('predictedDisplayTime', c_int64), ('predictedDisplayPeriod', c_int64), ('shouldRender', c_uint32)])
ViewLocateInfo = xr_struct('ViewLocateInfo', T_VIEW_LOCATE_INFO, [
    ('viewConfigurationType', c_int32), ('displayTime', c_int64), ('space', c_uint64)])
ViewState = xr_struct('ViewState', T_VIEW_STATE, [('viewStateFlags', c_uint64)])
View = xr_struct('View', T_VIEW, [('pose', Posef), ('fov', Fovf)])


class SwapchainSubImage(Structure):
    _fields_ = [('swapchain', c_uint64), ('imageRect', Rect2Di), ('imageArrayIndex', c_uint32)]


CompositionLayerProjectionView = xr_struct('CompositionLayerProjectionView', T_COMPOSITION_LAYER_PROJECTION_VIEW, [
    ('pose', Posef), ('fov', Fovf), ('subImage', SwapchainSubImage)])
CompositionLayerProjection = xr_struct('CompositionLayerProjection', T_COMPOSITION_LAYER_PROJECTION, [
    ('layerFlags', c_uint64), ('space', c_uint64), ('viewCount', c_uint32),
    ('views', POINTER(CompositionLayerProjectionView))])
CompositionLayerQuad = xr_struct('CompositionLayerQuad', T_COMPOSITION_LAYER_QUAD, [
    ('layerFlags', c_uint64), ('space', c_uint64), ('eyeVisibility', c_int32), ('subImage', SwapchainSubImage),
    ('pose', Posef), ('size', Extent2Df)])
CompositionLayerCylinder = xr_struct('CompositionLayerCylinder', T_COMPOSITION_LAYER_CYLINDER_KHR, [
    ('layerFlags', c_uint64), ('space', c_uint64), ('eyeVisibility', c_int32), ('subImage', SwapchainSubImage),
    ('pose', Posef), ('radius', c_float), ('centralAngle', c_float), ('aspectRatio', c_float)])
CompositionLayerEquirect2 = xr_struct('CompositionLayerEquirect2', T_COMPOSITION_LAYER_EQUIRECT2_KHR, [
    ('layerFlags', c_uint64), ('space', c_uint64), ('eyeVisibility', c_int32), ('subImage', SwapchainSubImage),
    ('pose', Posef), ('radius', c_float), ('centralHorizontalAngle', c_float), ('upperVerticalAngle', c_float),
    ('lowerVerticalAngle', c_float)])
CompositionLayerCube = xr_struct('CompositionLayerCube', T_COMPOSITION_LAYER_CUBE_KHR, [
    ('layerFlags', c_uint64), ('space', c_uint64), ('eyeVisibility', c_int32), ('swapchain', c_uint64),
    ('imageArrayIndex', c_uint32), ('orientation', Quat)])
CompositionLayerDepthInfo = xr_struct('CompositionLayerDepthInfo', T_COMPOSITION_LAYER_DEPTH_INFO_KHR, [
    ('subImage', SwapchainSubImage), ('minDepth', c_float), ('maxDepth', c_float), ('nearZ', c_float),
    ('farZ', c_float)])
CompositionLayerDepthTest = xr_struct('CompositionLayerDepthTest', T_COMPOSITION_LAYER_DEPTH_TEST_FB, [
    ('depthMask', c_uint32), ('compareOp', c_int32)])
FrameEndInfo = xr_struct('FrameEndInfo', T_FRAME_END_INFO, [
    ('displayTime', c_int64), ('environmentBlendMode', c_int32), ('layerCount', c_uint32),
    ('layers', POINTER(c_void_p))])
ActionSetCreateInfo = xr_struct('ActionSetCreateInfo', T_ACTION_SET_CREATE_INFO, [
    ('actionSetName', c_char * 64), ('localizedActionSetName', c_char * 128), ('priority', c_uint32)])
ActionCreateInfo = xr_struct('ActionCreateInfo', T_ACTION_CREATE_INFO, [
    ('actionName', c_char * 64), ('actionType', c_int32), ('countSubactionPaths', c_uint32),
    ('subactionPaths', POINTER(c_uint64)), ('localizedActionName', c_char * 128)])


class ActionSuggestedBinding(Structure):
    _fields_ = [('action', c_uint64), ('binding', c_uint64)]


InteractionProfileSuggestedBinding = xr_struct('InteractionProfileSuggestedBinding', T_INTERACTION_PROFILE_SUGGESTED_BINDING, [
    ('interactionProfile', c_uint64), ('countSuggestedBindings', c_uint32),
    ('suggestedBindings', POINTER(ActionSuggestedBinding))])
SessionActionSetsAttachInfo = xr_struct('SessionActionSetsAttachInfo', T_SESSION_ACTION_SETS_ATTACH_INFO, [
    ('countActionSets', c_uint32), ('actionSets', POINTER(c_uint64))])


class ActiveActionSet(Structure):
    _fields_ = [('actionSet', c_uint64), ('subactionPath', c_uint64)]


ActionsSyncInfo = xr_struct('ActionsSyncInfo', T_ACTIONS_SYNC_INFO, [
    ('countActiveActionSets', c_uint32), ('activeActionSets', POINTER(ActiveActionSet))])
ActionStateGetInfo = xr_struct('ActionStateGetInfo', T_ACTION_STATE_GET_INFO, [
    ('action', c_uint64), ('subactionPath', c_uint64)])
ActionStateBoolean = xr_struct('ActionStateBoolean', T_ACTION_STATE_BOOLEAN, [
    ('currentState', c_uint32), ('changedSinceLastSync', c_uint32), ('lastChangeTime', c_int64),
    ('isActive', c_uint32)])
ActionStateFloat = xr_struct('ActionStateFloat', T_ACTION_STATE_FLOAT, [
    ('currentState', c_float), ('changedSinceLastSync', c_uint32), ('lastChangeTime', c_int64),
    ('isActive', c_uint32)])
ActionStateVector2f = xr_struct('ActionStateVector2f', T_ACTION_STATE_VECTOR2F, [
    ('x', c_float), ('y', c_float), ('changedSinceLastSync', c_uint32), ('lastChangeTime', c_int64),
    ('isActive', c_uint32)])
ActionStatePose = xr_struct('ActionStatePose', T_ACTION_STATE_POSE, [('isActive', c_uint32)])
ActionSpaceCreateInfo = xr_struct('ActionSpaceCreateInfo', T_ACTION_SPACE_CREATE_INFO, [
    ('action', c_uint64), ('subactionPath', c_uint64), ('poseInActionSpace', Posef)])
InteractionProfileState = xr_struct('InteractionProfileState', T_INTERACTION_PROFILE_STATE, [
    ('interactionProfile', c_uint64)])
BoundSourcesForActionEnumerateInfo = xr_struct('BoundSourcesForActionEnumerateInfo', T_BOUND_SOURCES_FOR_ACTION_ENUMERATE_INFO, [
    ('action', c_uint64)])
InputSourceLocalizedNameGetInfo = xr_struct('InputSourceLocalizedNameGetInfo', T_INPUT_SOURCE_LOCALIZED_NAME_GET_INFO, [
    ('sourcePath', c_uint64), ('whichComponents', c_uint64)])
HapticActionInfo = xr_struct('HapticActionInfo', T_HAPTIC_ACTION_INFO, [('action', c_uint64), ('subactionPath', c_uint64)])
HapticVibration = xr_struct('HapticVibration', T_HAPTIC_VIBRATION, [
    ('duration', c_int64), ('frequency', c_float), ('amplitude', c_float)])
HandTrackerCreateInfo = xr_struct('HandTrackerCreateInfo', T_HAND_TRACKER_CREATE_INFO_EXT, [
    ('hand', c_int32), ('handJointSet', c_int32)])
HandJointsLocateInfo = xr_struct('HandJointsLocateInfo', T_HAND_JOINTS_LOCATE_INFO_EXT, [
    ('baseSpace', c_uint64), ('time', c_int64)])


class HandJointLocation(Structure):
    _fields_ = [('locationFlags', c_uint64), ('pose', Posef), ('radius', c_float)]


HandJointLocations = xr_struct('HandJointLocations', T_HAND_JOINT_LOCATIONS_EXT, [
    ('isActive', c_uint32), ('jointCount', c_uint32), ('jointLocations', POINTER(HandJointLocation))])


class NegotiateLoaderInfo(Structure):
    _fields_ = [('structType', c_uint32), ('structVersion', c_uint32), ('structSize', ctypes.c_size_t),
                ('minInterfaceVersion', c_uint32), ('maxInterfaceVersion', c_uint32),
                ('minApiVersion', c_uint64), ('maxApiVersion', c_uint64)]


class NegotiateRuntimeRequest(Structure):
    _fields_ = [('structType', c_uint32), ('structVersion', c_uint32), ('structSize', ctypes.c_size_t),
                ('runtimeInterfaceVersion', c_uint32), ('runtimeApiVersion', c_uint64),
                ('getInstanceProcAddr', c_void_p)]


def version(major, minor, patch):
    return (major << 48) | (minor << 32) | patch


class XrFunc(ctypes._CFuncPtr):
    """An OpenXR entry point: called with ctypes values (c_uint64 handles,
    byref structures), returns the XrResult as an int."""
    _flags_ = ctypes._FUNCFLAG_CDECL
    _restype_ = c_int32


# ---------------------------------------------------------------- D3D11 through COM vtables


class D3D11:
    """The few D3D11 calls the test renders with: views, clears, uploads."""

    def __init__(self):
        d3d11 = ctypes.WinDLL('d3d11')
        self.device, self.context, level = c_void_p(), c_void_p(), c_uint32()
        # Hardware driver, BGRA support, default feature levels, SDK version 7.
        hr = d3d11.D3D11CreateDevice(None, 1, None, 0x20, None, 0, 7, byref(self.device), byref(level),
                                     byref(self.context))
        if hr != 0:
            raise RuntimeError(f'D3D11CreateDevice failed: {hr & 0xffffffff:#x}')

    @staticmethod
    def _method(obj, index, restype, *argtypes):
        vtable = ctypes.cast(obj, POINTER(POINTER(c_void_p)))[0]
        return ctypes.WINFUNCTYPE(restype, c_void_p, *argtypes)(vtable[index])

    def rtv(self, texture, array_slice=None):
        desc = None
        if array_slice is not None:
            # D3D11_RENDER_TARGET_VIEW_DESC: Format, ViewDimension (TEXTURE2DARRAY), MipSlice, FirstArraySlice, ArraySize.
            desc = (c_uint32 * 5)(RGBA8, 5, 0, array_slice, 1)
        out = c_void_p()
        f = self._method(self.device, 9, c_long, c_void_p, c_void_p, POINTER(c_void_p))
        hr = f(self.device, texture, ctypes.cast(desc, c_void_p) if desc is not None else None, byref(out))
        if hr != 0:
            raise RuntimeError(f'CreateRenderTargetView failed: {hr & 0xffffffff:#x}')
        return out

    def dsv(self, texture):
        out = c_void_p()
        f = self._method(self.device, 10, c_long, c_void_p, c_void_p, POINTER(c_void_p))
        hr = f(self.device, texture, None, byref(out))
        if hr != 0:
            raise RuntimeError(f'CreateDepthStencilView failed: {hr & 0xffffffff:#x}')
        return out

    def clear(self, rtv, rgba):
        f = self._method(self.context, 50, None, c_void_p, POINTER(c_float))
        f(self.context, rtv, (c_float * 4)(*rgba))

    def clear_depth(self, dsv, value):
        f = self._method(self.context, 53, None, c_void_p, c_uint32, c_float, c_uint8)
        f(self.context, dsv, 1, value, 0)  # D3D11_CLEAR_DEPTH

    def fill(self, texture, x0, y0, x1, y1, rgba):
        """Paint a rectangle of an RGBA8 texture (UpdateSubresource)."""
        w, h = max(0, x1 - x0), max(0, y1 - y0)
        if w == 0 or h == 0:
            return
        px = bytes(int(round(c * 255)) for c in rgba)
        data = ctypes.create_string_buffer(px * (w * h), w * h * 4)
        box = (c_uint32 * 6)(x0, y0, 0, x1, y1, 1)
        f = self._method(self.context, 48, None, c_void_p, c_uint32, c_void_p, c_void_p, c_uint32, c_uint32)
        f(self.context, texture, 0, ctypes.cast(box, c_void_p), data, w * 4, w * h * 4)

    def flush(self):
        self._method(self.context, 111, None)(self.context)


# ---------------------------------------------------------------- the app


def check(r, what=''):
    if r < 0:
        raise RuntimeError(f'{what} -> {RESULT_NAME.get(r, r)}')
    return r


class Swapchain:
    def __init__(self, app, width, height, fmt=RGBA8, samples=1, faces=1, depth=False):
        self.app, self.width, self.height, self.fmt, self.faces = app, width, height, fmt, faces
        ci = SwapchainCreateInfo(usageFlags=USAGE_DEPTH if depth else USAGE_COLOR, format=fmt, sampleCount=samples,
                                 width=width, height=height, faceCount=faces, arraySize=1, mipCount=1)
        self.handle = c_uint64()
        check(app.f('xrCreateSwapchain')(app.session, byref(ci), byref(self.handle)), 'xrCreateSwapchain')
        n = c_uint32()
        check(app.f('xrEnumerateSwapchainImages')(self.handle, c_uint32(0), byref(n), None))
        images = (SwapchainImageD3D11 * n.value)(*[SwapchainImageD3D11() for _ in range(n.value)])
        check(app.f('xrEnumerateSwapchainImages')(self.handle, n, byref(n), images))
        self.textures = [c_void_p(i.texture) for i in images]
        d3d = app.d3d
        if depth:
            self.views = [d3d.dsv(t) for t in self.textures]
        elif faces == 6:
            self.views = [[d3d.rtv(t, f) for f in range(6)] for t in self.textures]
        else:
            self.views = [d3d.rtv(t) for t in self.textures]

    def paint(self, fn):
        """Acquire an image, let fn(view, texture) draw it, release it."""
        idx = c_uint32()
        check(self.app.f('xrAcquireSwapchainImage')(self.handle, None, byref(idx)), 'acquire')
        check(self.app.f('xrWaitSwapchainImage')(self.handle, byref(SwapchainImageWaitInfo(timeout=10**9))), 'wait')
        fn(self.views[idx.value], self.textures[idx.value])
        check(self.app.f('xrReleaseSwapchainImage')(self.handle, None), 'release')

    def sub(self, x=0, y=0, w=None, h=None):
        return SwapchainSubImage(self.handle.value, Rect2Di(x, y, w or self.width, h or self.height), 0)


DEFAULT_EXTENSIONS = [
    'XR_KHR_D3D11_enable', 'XR_KHR_win32_convert_performance_counter_time', 'XR_EXT_local_floor',
    'XR_MSFT_unbounded_reference_space',
    'XR_KHR_composition_layer_cylinder', 'XR_KHR_composition_layer_equirect2', 'XR_KHR_composition_layer_cube',
    'XR_KHR_composition_layer_depth', 'XR_FB_composition_layer_depth_test', 'XR_EXT_hand_tracking',
    'XR_EXT_hand_interaction',
]


class App:
    """One OpenXR app on XRMimic: instance, system, D3D11 device, session."""

    def __init__(self, extensions=DEFAULT_EXTENSIONS):
        self.lib = ctypes.CDLL(DLL)
        info = NegotiateLoaderInfo(1, 1, sizeof(NegotiateLoaderInfo), 1, 1, version(1, 0, 0), version(1, 0x3ff, 0xfff))
        req = NegotiateRuntimeRequest(3, 1, sizeof(NegotiateRuntimeRequest), 0, 0, None)
        check(self.lib.xrNegotiateLoaderRuntimeInterface(byref(info), byref(req)), 'negotiate')
        self.negotiated = (req.runtimeInterfaceVersion, req.runtimeApiVersion)
        self.gipa = XrFunc(req.getInstanceProcAddr)
        self.instance = c_uint64(0)
        self.funcs = {}
        self.events = []
        self.state = 0
        self.session = None
        self.d3d = None
        self.extensions = extensions

    def f(self, name):
        if name not in self.funcs:
            p = c_void_p()
            r = self.gipa(self.instance, c_char_p(name.encode()), byref(p))
            if r != 0 or not p.value:
                raise RuntimeError(f'{name}: {RESULT_NAME.get(r, r)}')
            self.funcs[name] = XrFunc(p.value)
        return self.funcs[name]

    def available_extensions(self):
        f = self.f('xrEnumerateInstanceExtensionProperties')
        n = c_uint32()
        check(f(None, c_uint32(0), byref(n), None))
        props = (ExtensionProperties * n.value)(*[ExtensionProperties() for _ in range(n.value)])
        check(f(None, n, byref(n), props))
        return {p.extensionName.decode(): p.extensionVersion for p in props}

    def create_instance(self, extensions=None, app_name=b'xrmimic python test'):
        if extensions is None:
            # What the device offers of the wanted ones (presets differ: hands are Meta's).
            offered = self.available_extensions()
            extensions = [e for e in self.extensions if e in offered]
        exts = [e.encode() for e in extensions]
        names = (c_char_p * len(exts))(*exts)
        ci = InstanceCreateInfo(applicationInfo=ApplicationInfo(app_name, 1, b'python', 1, version(1, 0, 0)),
                                enabledExtensionCount=len(exts), enabledExtensionNames=names)
        inst = c_uint64()
        r = self.f('xrCreateInstance')(byref(ci), byref(inst))
        if r == 0:
            self.instance = inst
            self.funcs = {k: v for k, v in self.funcs.items() if k in ('xrCreateInstance',)}
        return r

    def system(self):
        sid = c_uint64()
        check(self.f('xrGetSystem')(self.instance, byref(SystemGetInfo(formFactor=1)), byref(sid)), 'xrGetSystem')
        self.system_id = sid
        return sid

    def views_config(self):
        n = c_uint32()
        check(self.f('xrEnumerateViewConfigurationViews')(self.instance, self.system_id, c_int32(2), c_uint32(0), byref(n), None))
        vs = (ViewConfigurationView * n.value)(*[ViewConfigurationView() for _ in range(n.value)])
        check(self.f('xrEnumerateViewConfigurationViews')(self.instance, self.system_id, c_int32(2), n, byref(n), vs))
        return list(vs)

    def start(self):
        """Instance, system, device, session; polls until READY and begins it."""
        check(self.create_instance(), 'xrCreateInstance')
        self.system()
        req = GraphicsRequirementsD3D11()
        check(self.f('xrGetD3D11GraphicsRequirementsKHR')(self.instance, self.system_id, byref(req)))
        self.requirements = req
        self.d3d = D3D11()
        binding = GraphicsBindingD3D11(device=self.d3d.device.value)
        ci = SessionCreateInfo(next=ctypes.addressof(binding), systemId=self.system_id.value)
        self.session = c_uint64()
        check(self.f('xrCreateSession')(self.instance, byref(ci), byref(self.session)), 'xrCreateSession')
        vc = self.views_config()
        self.eye_w, self.eye_h = vc[0].recommendedImageRectWidth, vc[0].recommendedImageRectHeight
        self.wait_state('READY')
        check(self.f('xrBeginSession')(self.session, byref(SessionBeginInfo(primaryViewConfigurationType=2))), 'begin')
        self.local = self.space(SPACE_LOCAL)
        self.stage = self.space(SPACE_STAGE)

    def space(self, kind, p=None):
        h = c_uint64()
        r = self.f('xrCreateReferenceSpace')(self.session, byref(ReferenceSpaceCreateInfo(
            referenceSpaceType=kind, poseInReferenceSpace=p or pose())), byref(h))
        check(r, f'xrCreateReferenceSpace {kind}')
        return h

    def poll(self):
        while True:
            buf = EventDataBuffer()
            r = self.f('xrPollEvent')(self.instance, byref(buf))
            if r != 0:
                return
            if buf.type == T_EVENT_SESSION_STATE_CHANGED:
                e = EventSessionStateChanged.from_buffer(buf)
                self.state = e.state
                self.events.append(('state', STATE_NAMES[e.state]))
            elif buf.type == T_EVENT_REFERENCE_SPACE_CHANGE_PENDING:
                e = EventReferenceSpaceChangePending.from_buffer(buf)
                self.events.append(('space_change', e.referenceSpaceType, e.poseValid))
            elif buf.type == T_EVENT_INTERACTION_PROFILE_CHANGED:
                self.events.append(('profile_changed',))
            else:
                self.events.append(('event', buf.type))

    def wait_state(self, name, timeout=10.0):
        end = time.time() + timeout
        while time.time() < end:
            self.poll()
            if STATE_NAMES[self.state] == name:
                return
            time.sleep(0.005)
        raise RuntimeError(f'no {name} state (now {STATE_NAMES[self.state]})')

    def locate(self, space, base, t=0):
        loc = SpaceLocation()
        check(self.f('xrLocateSpace')(space, base, c_int64(t), byref(loc)), 'xrLocateSpace')
        return loc

    def path(self, s):
        p = c_uint64()
        check(self.f('xrStringToPath')(self.instance, c_char_p(s.encode()), byref(p)), f'path {s}')
        return p.value

    def path_str(self, p):
        n = c_uint32()
        buf = ctypes.create_string_buffer(256)
        check(self.f('xrPathToString')(self.instance, c_uint64(p), c_uint32(256), byref(n), buf))
        return buf.value.decode()

    def frame(self, render):
        """One frame: wait, begin, locate the views (LOCAL), render(views, state)
        returns the layers (structures, kept alive here), end."""
        fs = FrameState()
        check(self.f('xrWaitFrame')(self.session, None, byref(fs)), 'xrWaitFrame')
        check(self.f('xrBeginFrame')(self.session, None), 'xrBeginFrame')
        vli = ViewLocateInfo(viewConfigurationType=2, displayTime=fs.predictedDisplayTime, space=self.local.value)
        vs = ViewState()
        views = (View * 2)(View(), View())
        n = c_uint32()
        check(self.f('xrLocateViews')(self.session, byref(vli), byref(vs), c_uint32(2), byref(n), views), 'xrLocateViews')
        self.last_views = [(v.pose, v.fov) for v in views]
        layers = render(views, fs) or []
        ptrs = (c_void_p * max(1, len(layers)))(*[ctypes.addressof(l) for l in layers])
        fei = FrameEndInfo(displayTime=fs.predictedDisplayTime, environmentBlendMode=1, layerCount=len(layers), layers=ptrs)
        self.d3d.flush()
        check(self.f('xrEndFrame')(self.session, byref(fei)), 'xrEndFrame')
        self.poll()
        return fs

    def run(self, render, frames=None, snapshot=None):
        """Frames until `frames` are done, or the window's snapshot exists and
        45 more frames went by (at least 200, at most a minute)."""
        seen = None
        i = 0
        while True:
            if snapshot:
                if seen is None and os.path.exists(snapshot):
                    seen = i
                if (i >= 200 and seen is not None and i >= seen + 45) or i >= 90 * 60:
                    break
            elif i >= frames:
                break
            self.frame(lambda v, fs: render(i, v, fs))
            i += 1
        return i

    def stop(self):
        self.f('xrRequestExitSession')(self.session)
        try:
            self.wait_state('STOPPING', 5)
        except RuntimeError:
            pass
        self.f('xrEndSession')(self.session)
        self.f('xrDestroySession')(self.session)
        self.f('xrDestroyInstance')(self.instance)

    # -- layers
    def projection(self, swapchains, views, depth=None, stale=0.0):
        """A projection layer: per eye, its own swapchain (list of 2) and, if
        given, depth info (list of 2 CompositionLayerDepthInfo)."""
        pv = (CompositionLayerProjectionView * 2)()
        for e in range(2):
            p = Posef.from_buffer_copy(views[e].pose)
            p.position.x -= stale
            pv[e] = CompositionLayerProjectionView(pose=p, fov=views[e].fov, subImage=swapchains[e].sub())
            if depth:
                pv[e].next = ctypes.addressof(depth[e])
        layer = CompositionLayerProjection(space=self.local.value, viewCount=2, views=pv)
        layer._keep = (pv, depth)
        return layer

    def depth_info(self, swapchain, metres, near=0.1, far=100.0):
        def draw(dsv, tex):
            z = far / (far - near) * (1.0 - near / metres)
            self.d3d.clear_depth(dsv, z)
        swapchain.paint(draw)
        return CompositionLayerDepthInfo(subImage=swapchain.sub(), minDepth=0.0, maxDepth=1.0, nearZ=near, farZ=far)


# ---------------------------------------------------------------- picture checks


def read_bmp(path):
    with open(path, 'rb') as fh:
        b = fh.read()
    off = int.from_bytes(b[10:14], 'little')
    w = int.from_bytes(b[18:22], 'little', signed=True)
    h = int.from_bytes(b[22:26], 'little', signed=True)
    bpp = int.from_bytes(b[28:30], 'little')
    top_down = h < 0
    h = abs(h)
    stride = ((w * bpp // 8) + 3) & ~3

    def px(x, y):
        x, y = int(round(x)), int(round(y))
        if not (0 <= x < w and 0 <= y < h):
            return None
        row = y if top_down else h - 1 - y
        o = off + row * stride + x * (bpp // 8)
        return (b[o + 2], b[o + 1], b[o])
    return w, h, px


def qrot(q, v):
    """Rotate v by quaternion q (x, y, z, w)."""
    x, y, z, w = q
    tx, ty, tz = 2 * (y * v[2] - z * v[1]), 2 * (z * v[0] - x * v[2]), 2 * (x * v[1] - y * v[0])
    return (v[0] + w * tx + (y * tz - z * ty), v[1] + w * ty + (z * tx - x * tz), v[2] + w * tz + (x * ty - y * tx))


class Window:
    """Where things land in the headset window's snapshot: the view sits under
    the 34 px toolbar, letterboxed to both eyes side by side."""

    def __init__(self, path, eye_w, eye_h):
        self.w, self.h, self.px = read_bmp(path)
        vw0, vh0 = self.w, self.h - 34
        aspect = eye_w * 2 / eye_h
        vw, vh = vw0, vw0 / aspect
        if vh > vh0:
            vh, vw = vh0, vh0 * aspect
        self.vx0, self.vy0, self.vw0, self.vh0 = 0, 34, vw0, vh0
        self.x0, self.y0, self.vw, self.vh = (vw0 - vw) / 2, 34 + (vh0 - vh) / 2, vw, vh

    def eye_point(self, e, view, p):
        """Pixel of world point p (LOCAL space) in eye e drawn from view (pose, fov)."""
        ps, fov = view
        q = (ps.orientation.x, ps.orientation.y, ps.orientation.z, ps.orientation.w)
        d = (p[0] - ps.position.x, p[1] - ps.position.y, p[2] - ps.position.z)
        l = qrot((-q[0], -q[1], -q[2], q[3]), d)
        tx, ty = l[0] / -l[2], l[1] / -l[2]
        tl, tr, tu, td = math.tan(fov.left), math.tan(fov.right), math.tan(fov.up), math.tan(fov.down)
        u, v = (tx - tl) / (tr - tl), (tu - ty) / (tu - td)
        return self.x0 + e * self.vw / 2 + u * self.vw / 2, self.y0 + v * self.vh

    def eye_dir(self, e, view, d):
        """Pixel of a direction (at infinity) in eye e."""
        ps = view[0]
        return self.eye_point(e, view, (ps.position.x + d[0] * 1e4, ps.position.y + d[1] * 1e4, ps.position.z + d[2] * 1e4))


class Checks:
    def __init__(self):
        self.items = []

    def ok(self, what, good, got='', want=''):
        self.items.append([what, bool(good), str(got), str(want)])

    def eq(self, what, got, want):
        self.ok(what, got == want, got, want)

    def near(self, what, got, want, tol):
        self.ok(what, got is not None and abs(got - want) <= tol, got, f'{want} ± {tol}')

    def colour(self, what, got, want, tol=40):
        self.ok(what, got is not None and all(abs(a - b) <= tol for a, b in zip(got, want)), got, want)


def snapshot_path():
    return os.environ.get('XRMIMIC_TEST_SNAPSHOT')


RED, GREEN, BLUE, YELLOW, MAGENTA, CYAN, WHITE, BLACK = (
    (1, 0, 0, 1), (0, 1, 0, 1), (0, 0, 1, 1), (1, 1, 0, 1), (1, 0, 1, 1), (0, 1, 1, 1), (1, 1, 1, 1), (0, 0, 0, 1))


def rgb(c):
    return tuple(int(round(v * 255)) for v in c[:3])


def eye_swapchains(app, samples=1):
    return [Swapchain(app, app.eye_w, app.eye_h, samples=samples) for _ in range(2)]


def paint_eyes(app, chains, colours):
    for sc, c in zip(chains, colours):
        sc.paint(lambda v, t, c=c: app.d3d.clear(v, c))


def solid(app, w, h, colour, samples=1):
    sc = Swapchain(app, w, h, samples=samples)
    sc.colour = colour
    return sc


def quad(app, sc, p, size, eyes=EYES_BOTH, flags=0):
    sc.paint(lambda v, t: app.d3d.clear(v, sc.colour))
    return CompositionLayerQuad(layerFlags=flags, space=app.local.value, eyeVisibility=eyes, subImage=sc.sub(),
                                pose=p, size=Extent2Df(size, size))


# ---------------------------------------------------------------- scenarios
# Each runs in its own process and returns its checks.

SCENARIOS = {}


def scenario(fn):
    SCENARIOS[fn.__name__.removeprefix('s_')] = fn
    return fn


@scenario
def s_api():
    """The API without pictures: everything an app asks before and while it runs."""
    c = Checks()
    app = App()
    c.ok('loader negotiation: interface 1', app.negotiated[0] == 1, app.negotiated)
    exts = app.available_extensions()
    missing = [e for e in DEFAULT_EXTENSIONS + ['XR_EXT_win32_appcontainer_compatible'] if e not in exts]
    c.ok('extensions offered (Quest 2 preset)', not missing, f'missing {missing}' if missing else len(exts))
    c.eq('unknown extension refused', RESULT_NAME.get(app.create_instance(['XR_FAKE_not_real'])), 'ERROR_EXTENSION_NOT_PRESENT')
    app.start()
    ip = InstanceProperties()
    check(app.f('xrGetInstanceProperties')(app.instance, byref(ip)))
    c.eq('runtime name', ip.runtimeName.decode(), 'XRMimic')
    sp = SystemProperties()
    check(app.f('xrGetSystemProperties')(app.instance, app.system_id, byref(sp)))
    c.eq('system name (Quest 2 preset)', sp.systemName.decode(), 'Oculus Quest2')
    c.eq('vendor id', hex(sp.vendorId), '0x2833')
    c.ok('orientation and position tracking', sp.orientationTracking and sp.positionTracking)
    c.ok('layer count at least 16', sp.maxLayerCount >= 16, sp.maxLayerCount)
    vc = app.views_config()
    c.eq('two views', len(vc), 2)
    c.eq('recommended eye size', (vc[0].recommendedImageRectWidth, vc[0].recommendedImageRectHeight), (1832, 1920))
    c.eq('max MSAA samples', vc[0].maxSwapchainSampleCount, 4)
    n = c_uint32()
    modes = (c_int32 * 4)()
    check(app.f('xrEnumerateEnvironmentBlendModes')(app.instance, app.system_id, c_int32(2), c_uint32(4), byref(n), modes))
    c.eq('blend modes', list(modes[:n.value]), [1])
    c.ok('D3D11 feature level 11.0 or less required', 0 < app.requirements.minFeatureLevel <= 0xb000,
         hex(app.requirements.minFeatureLevel))
    buf = ctypes.create_string_buffer(64)
    check(app.f('xrResultToString')(app.instance, c_int32(-27), buf))
    c.eq('xrResultToString', buf.value.decode(), 'XR_ERROR_ACTION_TYPE_MISMATCH')
    check(app.f('xrStructureTypeToString')(app.instance, c_int32(T_FRAME_STATE), buf))
    c.eq('xrStructureTypeToString', buf.value.decode(), 'XR_TYPE_FRAME_STATE')
    p = app.path('/user/hand/left')
    c.eq('path round trip', app.path_str(p), '/user/hand/left')
    bad = c_uint64()
    c.eq('malformed path refused', RESULT_NAME.get(app.f('xrStringToPath')(app.instance, c_char_p(b'no/leading/slash'), byref(bad))),
         'ERROR_PATH_FORMAT_INVALID')
    # The session got to FOCUSED through its states.
    app.poll()
    c.eq('session states', [e[1] for e in app.events if e[0] == 'state'][:5], ['IDLE', 'READY', 'SYNCHRONIZED', 'VISIBLE', 'FOCUSED'])
    # Reference spaces.
    spaces = (c_int32 * 8)()
    check(app.f('xrEnumerateReferenceSpaces')(app.session, c_uint32(8), byref(n), spaces))
    c.ok('reference spaces', {SPACE_VIEW, SPACE_LOCAL, SPACE_STAGE, SPACE_LOCAL_FLOOR, SPACE_UNBOUNDED_MSFT} <= set(spaces[:n.value]), list(spaces[:n.value]))
    floor = app.space(SPACE_LOCAL_FLOOR)
    view = app.space(SPACE_VIEW)
    c.near('LOCAL starts at eye height above STAGE', app.locate(app.local, app.stage).pose.position.y, 1.6, 1e-3)
    c.near('LOCAL_FLOOR is on the floor', app.locate(floor, app.stage).pose.position.y, 0.0, 1e-3)
    unbounded = app.space(SPACE_UNBOUNDED_MSFT)
    c.near('UNBOUNDED_MSFT starts where the head started, as LOCAL', app.locate(unbounded, app.stage).pose.position.y, 1.6, 1e-3)
    r = app.f('xrGetReferenceSpaceBoundsRect')(app.session, c_int32(SPACE_UNBOUNDED_MSFT), byref(Extent2Df()))
    c.eq('UNBOUNDED_MSFT has no bounds', RESULT_NAME.get(r), 'SPACE_BOUNDS_UNAVAILABLE')
    loc = app.locate(view, app.local)
    c.ok('VIEW located, tracked', loc.locationFlags & 3 == 3, loc.locationFlags)
    bounds = Extent2Df()
    r = app.f('xrGetReferenceSpaceBoundsRect')(app.session, c_int32(SPACE_STAGE), byref(bounds))
    c.eq('STAGE bounds: the play area', (RESULT_NAME.get(r), bounds.width, bounds.height), ('SUCCESS', 2.5, 2.5))
    r = app.f('xrGetReferenceSpaceBoundsRect')(app.session, c_int32(SPACE_LOCAL), byref(bounds))
    c.eq('LOCAL has no bounds', RESULT_NAME.get(r), 'SPACE_BOUNDS_UNAVAILABLE')
    bad_space = c_uint64()
    c.eq('unknown reference space refused', RESULT_NAME.get(app.f('xrCreateReferenceSpace')(
        app.session, byref(ReferenceSpaceCreateInfo(referenceSpaceType=1000039001, poseInReferenceSpace=pose())), byref(bad_space))),
        'ERROR_REFERENCE_SPACE_UNSUPPORTED')
    # Swapchains.
    formats = (c_int64 * 32)()
    check(app.f('xrEnumerateSwapchainFormats')(app.session, c_uint32(32), byref(n), formats))
    c.ok('swapchain formats: RGBA8 sRGB, RGBA8, D32', {RGBA8, RGBA8_SRGB, D32_FLOAT} <= set(formats[:n.value]), list(formats[:n.value]))

    def make(**kw):
        ci = SwapchainCreateInfo(**{**dict(usageFlags=USAGE_COLOR, format=RGBA8, sampleCount=1, width=64, height=64,
                                          faceCount=1, arraySize=1, mipCount=1), **kw})
        h = c_uint64()
        return RESULT_NAME.get(app.f('xrCreateSwapchain')(app.session, byref(ci), byref(h)))
    c.eq('4x MSAA colour swapchain', make(sampleCount=4), 'SUCCESS')
    c.eq('4x MSAA depth swapchain', make(sampleCount=4, format=D32_FLOAT, usageFlags=USAGE_DEPTH), 'SUCCESS')
    c.eq('cube swapchain', make(faceCount=6), 'SUCCESS')
    c.eq('16x MSAA refused', make(sampleCount=16), 'ERROR_FEATURE_UNSUPPORTED')
    c.eq('MSAA cube refused', make(sampleCount=4, faceCount=6), 'ERROR_FEATURE_UNSUPPORTED')
    c.eq('unknown format refused', make(format=2), 'ERROR_SWAPCHAIN_FORMAT_UNSUPPORTED')
    sc = Swapchain(app, 64, 64)
    c.eq('release without acquire refused', RESULT_NAME.get(app.f('xrReleaseSwapchainImage')(sc.handle, None)), 'ERROR_CALL_ORDER_INVALID')
    # End before stopping is refused.
    c.eq('xrEndSession while running refused', RESULT_NAME.get(app.f('xrEndSession')(app.session)), 'ERROR_SESSION_NOT_STOPPING')
    # Actions: Touch bindings, held test input (XRMIMIC_TEST_CONTROLLERS).
    acts = Actions(app)
    # Frames, paced at the headset's rate.
    eyes = eye_swapchains(app)
    times = []

    def render(i, views, fs):
        times.append((fs.predictedDisplayTime, fs.predictedDisplayPeriod, time.perf_counter()))
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]
    app.run(render, frames=150)
    period = times[-1][1] / 1e6
    c.near('display period (90 Hz)', period, 1000 / 90, 0.2)
    c.ok('display times increase', all(b[0] > a[0] for a, b in zip(times, times[1:])))
    # xrWaitFrame holds the app to the headset's rate: never faster (a busy
    # machine may make this Python app slower, which is the app's doing).
    wall = (times[-1][2] - times[50][2]) / (len(times) - 51) * 1000
    c.ok('frames paced at the period, not faster (ms)', wall >= 1000 / 90 * 0.95, round(wall, 2), '>= 10.6')
    acts.sync()
    c.eq('interaction profile', acts.profile(1), '/interaction_profiles/oculus/touch_controller')
    c.near('right trigger (held by the test)', acts.float(acts.trigger, 1), 1.0, 1e-3)
    c.ok('right A pressed', acts.bool(acts.primary, 1))
    c.near('left squeeze', acts.float(acts.squeeze, 0), 1.0, 1e-3)
    st = acts.vec2(acts.stick, 0)
    c.ok('left thumbstick (0.5, -1)', abs(st[0] - 0.5) < 1e-3 and abs(st[1] + 1) < 1e-3, st)
    c.near('trigger without subaction path: the larger hand', acts.float(acts.trigger, None), 1.0, 1e-3)
    c.eq('float read from a boolean action refused', RESULT_NAME.get(acts.float_result(acts.primary, 1)), 'ERROR_ACTION_TYPE_MISMATCH')
    grip = acts.grip_space(1)
    gl = app.locate(grip, app.local)
    c.ok('right grip located', gl.locationFlags & 3 == 3, gl.locationFlags)
    c.ok('right grip below and right of the head', gl.pose.position.x > 0.05 and gl.pose.position.y < -0.1, (gl.pose.position.x, gl.pose.position.y))
    srcs = acts.bound_sources(acts.trigger)
    c.eq('trigger bound sources', sorted(app.path_str(s) for s in srcs),
         ['/user/hand/left/input/trigger/value', '/user/hand/right/input/trigger/value'])
    c.ok('localized source name', acts.source_name(srcs[0]) != '', acts.source_name(srcs[0]))
    # Haptics.
    c.eq('haptic pulse', RESULT_NAME.get(acts.pulse(1, 0.8, 100_000_000)), 'SUCCESS')
    c.eq('haptic on an input action refused', RESULT_NAME.get(acts.pulse(1, 0.8, 100_000_000, action=acts.trigger)), 'ERROR_ACTION_TYPE_MISMATCH')
    c.eq('haptic stop', RESULT_NAME.get(app.f('xrStopHapticFeedback')(app.session, byref(HapticActionInfo(action=acts.buzz.value)))), 'SUCCESS')
    # Time conversion (QPC).
    qpc = c_int64()
    ctypes.windll.kernel32.QueryPerformanceCounter(byref(qpc))
    t = c_int64()
    check(app.f('xrConvertWin32PerformanceCounterToTimeKHR')(app.instance, byref(qpc), byref(t)))
    back = c_int64()
    check(app.f('xrConvertTimeToWin32PerformanceCounterKHR')(app.instance, t, byref(back)))
    c.ok('QPC -> XrTime -> QPC', abs(back.value - qpc.value) <= 2, (qpc.value, back.value))
    c.ok('XrTime near the frame times', abs(t.value - times[-1][0]) < 2e9, (t.value, times[-1][0]))
    # Stop.
    check(app.f('xrRequestExitSession')(app.session))
    app.wait_state('STOPPING')
    check(app.f('xrEndSession')(app.session))
    app.poll()
    c.eq('session ends: IDLE, EXITING', [e[1] for e in app.events if e[0] == 'state'][-3:], ['STOPPING', 'IDLE', 'EXITING'])
    check(app.f('xrDestroySession')(app.session))
    check(app.f('xrDestroyInstance')(app.instance))
    return c


class Actions:
    """One action set with Touch, simple, Index and EXT hand bindings."""

    def __init__(self, app, extra_profiles=()):
        self.app = app
        f = app.f
        ci = ActionSetCreateInfo(actionSetName=b'test', localizedActionSetName=b'Test', priority=0)
        self.set = c_uint64()
        check(f('xrCreateActionSet')(app.instance, byref(ci), byref(self.set)))
        self.hands = (c_uint64 * 2)(app.path('/user/hand/left'), app.path('/user/hand/right'))

        def action(name, kind):
            a = ActionCreateInfo(actionName=name.encode(), actionType=kind, countSubactionPaths=2,
                                 subactionPaths=self.hands, localizedActionName=name.encode())
            h = c_uint64()
            check(f('xrCreateAction')(self.set, byref(a), byref(h)), f'action {name}')
            return h
        self.trigger = action('trigger', FLOAT_INPUT)
        self.squeeze = action('squeeze', FLOAT_INPUT)
        self.stick = action('stick', VECTOR2F_INPUT)
        self.primary = action('primary', BOOLEAN_INPUT)
        self.grip = action('grip', POSE_INPUT)
        self.buzz = action('buzz', VIBRATION_OUTPUT)

        def suggest(profile, pairs):
            b = (ActionSuggestedBinding * len(pairs))(*[ActionSuggestedBinding(a.value, app.path(p)) for a, p in pairs])
            check(f('xrSuggestInteractionProfileBindings')(app.instance, byref(InteractionProfileSuggestedBinding(
                interactionProfile=app.path(profile), countSuggestedBindings=len(pairs), suggestedBindings=b))), profile)
        for profile, (trig, sq, st, prim) in {
            '/interaction_profiles/oculus/touch_controller': ('trigger/value', 'squeeze/value', 'thumbstick', 'a/click'),
            '/interaction_profiles/valve/index_controller': ('trigger/value', 'squeeze/value', 'thumbstick', 'a/click'),
        }.items():
            pairs = []
            for side in ('left', 'right'):
                pairs += [(self.trigger, f'/user/hand/{side}/input/{trig}'), (self.squeeze, f'/user/hand/{side}/input/{sq}'),
                          (self.stick, f'/user/hand/{side}/input/{st}'), (self.grip, f'/user/hand/{side}/input/grip/pose'),
                          (self.buzz, f'/user/hand/{side}/output/haptic')]
            pairs += [(self.primary, '/user/hand/right/input/' + prim), (self.primary, '/user/hand/left/input/x/click'
                       if 'touch' in profile else '/user/hand/left/input/a/click')]
            suggest(profile, pairs)
        hand = []
        for side in ('left', 'right'):
            hand += [(self.trigger, f'/user/hand/{side}/input/pinch_ext/value'),
                     (self.squeeze, f'/user/hand/{side}/input/grasp_ext/value'),
                     (self.grip, f'/user/hand/{side}/input/grip/pose')]
        suggest('/interaction_profiles/ext/hand_interaction_ext', hand)
        sets = (c_uint64 * 1)(self.set.value)
        check(f('xrAttachSessionActionSets')(app.session, byref(SessionActionSetsAttachInfo(countActionSets=1, actionSets=sets))))

    def sync(self):
        active = (ActiveActionSet * 1)(ActiveActionSet(self.set.value, 0))
        return self.app.f('xrSyncActions')(self.app.session, byref(ActionsSyncInfo(countActiveActionSets=1, activeActionSets=active)))

    def _get(self, action, hand):
        return ActionStateGetInfo(action=action.value, subactionPath=0 if hand is None else self.hands[hand])

    def float_result(self, action, hand):
        s = ActionStateFloat()
        return self.app.f('xrGetActionStateFloat')(self.app.session, byref(self._get(action, hand)), byref(s))

    def float(self, action, hand):
        s = ActionStateFloat()
        check(self.app.f('xrGetActionStateFloat')(self.app.session, byref(self._get(action, hand)), byref(s)))
        return round(s.currentState, 4)

    def bool(self, action, hand):
        s = ActionStateBoolean()
        check(self.app.f('xrGetActionStateBoolean')(self.app.session, byref(self._get(action, hand)), byref(s)))
        return bool(s.currentState)

    def vec2(self, action, hand):
        s = ActionStateVector2f()
        check(self.app.f('xrGetActionStateVector2f')(self.app.session, byref(self._get(action, hand)), byref(s)))
        return (round(s.x, 4), round(s.y, 4))

    def profile(self, hand):
        s = InteractionProfileState()
        check(self.app.f('xrGetCurrentInteractionProfile')(self.app.session, c_uint64(self.hands[hand]), byref(s)))
        return self.app.path_str(s.interactionProfile) if s.interactionProfile else ''

    def grip_space(self, hand):
        h = c_uint64()
        check(self.app.f('xrCreateActionSpace')(self.app.session, byref(ActionSpaceCreateInfo(
            action=self.grip.value, subactionPath=self.hands[hand], poseInActionSpace=pose())), byref(h)))
        return h

    def bound_sources(self, action):
        n = c_uint32()
        info = BoundSourcesForActionEnumerateInfo(action=action.value)
        check(self.app.f('xrEnumerateBoundSourcesForAction')(self.app.session, byref(info), c_uint32(0), byref(n), None))
        out = (c_uint64 * n.value)()
        check(self.app.f('xrEnumerateBoundSourcesForAction')(self.app.session, byref(info), n, byref(n), out))
        return list(out)

    def source_name(self, src):
        n = c_uint32()
        buf = ctypes.create_string_buffer(256)
        info = InputSourceLocalizedNameGetInfo(sourcePath=src, whichComponents=0x7)
        check(self.app.f('xrGetInputSourceLocalizedName')(self.app.session, byref(info), c_uint32(256), byref(n), buf))
        return buf.value.decode()

    def pulse(self, hand, amplitude, duration_ns, action=None):
        info = HapticActionInfo(action=(action or self.buzz).value, subactionPath=self.hands[hand])
        v = HapticVibration(duration=duration_ns, frequency=160.0, amplitude=amplitude)
        return self.app.f('xrApplyHapticFeedback')(self.app.session, byref(info), byref(v))


@scenario
def s_motion():
    """The simulated user walks and turns (XRMIMIC_TEST_MOVE / _TURN)."""
    c = Checks()
    app = App()
    app.start()
    view = app.space(SPACE_VIEW)
    eyes = eye_swapchains(app)
    start = app.locate(view, app.stage).pose
    t0 = time.perf_counter()

    def render(i, views, fs):
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]
    app.run(render, frames=180)
    secs = time.perf_counter() - t0
    end = app.locate(view, app.stage).pose
    moved = math.hypot(end.position.x - start.position.x, end.position.z - start.position.z)
    fwd = lambda q: qrot((q.x, q.y, q.z, q.w), (0, 0, -1))
    a, b = fwd(start.orientation), fwd(end.orientation)
    turned = abs(math.atan2(a[0] * b[2] - a[2] * b[0], a[0] * b[0] + a[2] * b[2]))
    c.near('walked at 1.4 m/s (m)', moved, 1.4 * secs, 0.35 * secs)
    c.near('turned at 0.5 rad/s (rad)', turned, 0.5 * secs, 0.15 * secs)
    c.near('head height kept', end.position.y, 1.6, 0.01)
    app.stop()
    return c


@scenario
def s_placed():
    """A controller put down apart from the head stays while the head walks."""
    c = Checks()
    app = App()
    app.start()
    acts = Actions(app)
    grips = [acts.grip_space(0), acts.grip_space(1)]
    eyes = eye_swapchains(app)
    first = {}

    def render(i, views, fs):
        acts.sync()
        if i == 5:
            first['left'] = app.locate(grips[0], app.stage).pose.position
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]
    app.run(render, frames=180)
    r = app.locate(grips[1], app.stage).pose.position
    l = app.locate(grips[0], app.stage).pose.position
    c.ok('right controller where it was put (0.3, 1.0, -0.5)', abs(r.x - 0.3) < 0.01 and abs(r.y - 1.0) < 0.01 and abs(r.z + 0.5) < 0.01,
         (round(r.x, 3), round(r.y, 3), round(r.z, 3)))
    fl = first['left']
    c.ok('left controller walked with the head', math.hypot(l.x - fl.x, l.z - fl.z) > 1.0, round(math.hypot(l.x - fl.x, l.z - fl.z), 2))
    app.stop()
    return c


@scenario
def s_input_tab():
    """Inputs held through the window's Input tab (XRMIMIC_TEST_INPUT) reach the actions."""
    c = Checks()
    app = App()
    app.start()
    acts = Actions(app)
    eyes = eye_swapchains(app)

    def render(i, views, fs):
        acts.sync()
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]
    app.run(render, frames=240)
    c.near('right trigger 0.7', acts.float(acts.trigger, 1), 0.7, 0.011)
    c.ok('right A pressed', acts.bool(acts.primary, 1))
    st = acts.vec2(acts.stick, 0)
    c.ok('left stick (0.5, -0.25)', abs(st[0] - 0.5) < 0.011 and abs(st[1] + 0.25) < 0.011, st)
    c.near('left trigger untouched', acts.float(acts.trigger, 0), 0.0, 1e-3)
    app.stop()
    return c


@scenario
def s_hands():
    """Controllers put down: the hand profile, 26 joints per hand, a pinch."""
    c = Checks()
    app = App()
    app.start()
    acts = Actions(app)
    trackers = []
    for hand in (1, 2):
        h = c_uint64()
        check(app.f('xrCreateHandTrackerEXT')(app.session, byref(HandTrackerCreateInfo(hand=hand, handJointSet=0)), byref(h)))
        trackers.append(h)
    eyes = eye_swapchains(app)
    joints = [None, None]

    def render(i, views, fs):
        acts.sync()
        for k, t in enumerate(trackers):
            locs = (HandJointLocation * 26)()
            hl = HandJointLocations(jointCount=26, jointLocations=locs)
            check(app.f('xrLocateHandJointsEXT')(t, byref(HandJointsLocateInfo(baseSpace=app.stage.value, time=fs.predictedDisplayTime)), byref(hl)))
            joints[k] = [(l.pose.position.x, l.pose.position.y, l.pose.position.z, l.radius) for l in locs] if hl.isActive else None
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]
    app.run(render, frames=240)
    c.eq('hand interaction profile', acts.profile(1), '/interaction_profiles/ext/hand_interaction_ext')
    c.ok('profile change announced', ('profile_changed',) in app.events, app.events[-5:])
    c.ok('both hands tracked', joints[0] is not None and joints[1] is not None)
    if joints[1]:
        d = lambda a, b: math.dist(joints[1][a][:3], joints[1][b][:3])
        c.ok('right pinch: thumb tip to index tip under 15 mm', d(5, 10) < 0.015, round(d(5, 10), 4))
        c.ok('joint radii set', all(j[3] > 0.003 for j in joints[1]))
    c.near('pinch drives the trigger action', acts.float(acts.trigger, 1), 1.0, 0.011)
    app.stop()
    return c


@scenario
def s_device():
    """Another device preset (XRMIMIC_DEVICE=index): name, size, play area, controllers."""
    c = Checks()
    app = App()
    app.start()
    sp = SystemProperties()
    check(app.f('xrGetSystemProperties')(app.instance, app.system_id, byref(sp)))
    c.eq('system name', sp.systemName.decode(), 'Valve Index')
    c.eq('eye size', (app.eye_w, app.eye_h), (2016, 2240))
    bounds = Extent2Df()
    check(app.f('xrGetReferenceSpaceBoundsRect')(app.session, c_int32(SPACE_STAGE), byref(bounds)))
    c.eq('room-scale play area', (bounds.width, bounds.height), (3.5, 3.0))
    acts = Actions(app)
    eyes = eye_swapchains(app)

    def render(i, views, fs):
        acts.sync()
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]
    app.run(render, frames=60)
    c.eq('interaction profile', acts.profile(1), '/interaction_profiles/valve/index_controller')
    app.stop()
    return c


@scenario
def s_stage_change():
    """The play area changes mid-session (the Device tab applies the Index preset)."""
    c = Checks()
    app = App()
    app.start()
    bounds = Extent2Df()
    check(app.f('xrGetReferenceSpaceBoundsRect')(app.session, c_int32(SPACE_STAGE), byref(bounds)))
    before = (bounds.width, bounds.height)
    eyes = eye_swapchains(app)

    def render(i, views, fs):
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]
    app.run(render, frames=200)
    check(app.f('xrGetReferenceSpaceBoundsRect')(app.session, c_int32(SPACE_STAGE), byref(bounds)))
    c.eq('bounds before', before, (2.5, 2.5))
    changes = [e for e in app.events if e[0] == 'space_change']
    c.ok('STAGE change event', any(e[1] == SPACE_STAGE and e[2] for e in changes), changes)
    c.eq('bounds after', (bounds.width, bounds.height), (3.5, 3.0))
    app.stop()
    return c


@scenario
def s_layers():
    """Projection, quads (both eyes, left only), cylinder and equirect in the window;
    the frame capture holds the projection layer alone. An equirect with an empty
    band (upper angle below lower) is left out and said so in the log."""
    c = Checks()
    samples = 4 if os.environ.get('XRMIMIC_PY_MSAA') else 1
    app = App()
    app.start()
    eyes = eye_swapchains(app, samples)
    blue = solid(app, 64, 64, BLUE, samples)
    white = solid(app, 64, 64, WHITE, samples)
    magenta = solid(app, 128, 32, MAGENTA, samples)
    yellow = solid(app, 64, 32, YELLOW, samples)
    quad_a = pose(0, 0, -2)
    quad_b = pose(0.7, 0, -2)
    cyl = dict(radius=2.5, centralAngle=0.5, aspectRatio=4.0, y=-0.7)
    eq = dict(centralHorizontalAngle=0.8, upperVerticalAngle=0.75, lowerVerticalAngle=0.45)

    def render(i, views, fs):
        paint_eyes(app, eyes, [RED, GREEN])
        magenta.paint(lambda v, t: app.d3d.clear(v, magenta.colour))
        yellow.paint(lambda v, t: app.d3d.clear(v, yellow.colour))
        cl = CompositionLayerCylinder(space=app.local.value, eyeVisibility=EYES_BOTH, subImage=magenta.sub(),
                                      pose=pose(0, cyl['y'], 0), radius=cyl['radius'], centralAngle=cyl['centralAngle'],
                                      aspectRatio=cyl['aspectRatio'])
        el = CompositionLayerEquirect2(space=app.local.value, eyeVisibility=EYES_BOTH, subImage=yellow.sub(), pose=pose(),
                                       radius=0.0, **eq)
        empty = CompositionLayerEquirect2(space=app.local.value, eyeVisibility=EYES_BOTH, subImage=yellow.sub(), pose=pose(),
                                          radius=0.0, centralHorizontalAngle=0.8, upperVerticalAngle=-0.2,
                                          lowerVerticalAngle=0.2)
        return [app.projection(eyes, views), el, empty, cl, quad(app, blue, quad_a, 0.6),
                quad(app, white, quad_b, 0.3, EYES_LEFT)]
    app.run(render, snapshot=snapshot_path())
    views = app.last_views
    app.stop()
    win = Window(snapshot_path(), app.eye_w, app.eye_h)
    for e, name in ((0, 'left'), (1, 'right')):
        base = rgb(RED if e == 0 else GREEN)
        c.colour(f'{name} eye: projection beside the quad', win.px(*win.eye_point(e, views[e], (-0.55, 0.25, -2))), base)
        c.colour(f'{name} eye: quad', win.px(*win.eye_point(e, views[e], (0, 0, -2))), rgb(BLUE))
        c.colour(f'{name} eye: cylinder', win.px(*win.eye_point(e, views[e], (0, cyl['y'], -cyl['radius']))), rgb(MAGENTA))
        el, ang = 0.6, 0.0
        c.colour(f'{name} eye: equirect band at infinity', win.px(*win.eye_dir(e, views[e], (0, math.sin(el), -math.cos(el)))), rgb(YELLOW))
    c.colour('left eye: left-eye-only quad', win.px(*win.eye_point(0, views[0], (0.7, 0, -2))), rgb(WHITE))
    c.colour('right eye: no left-eye-only quad', win.px(*win.eye_point(1, views[1], (0.7, 0, -2))), rgb(GREEN))
    with open(os.environ['XRMIMIC_LOG'], encoding='utf-8', errors='replace') as fh:
        said = [l for l in fh if 'equirect2 layer not drawn' in l]
    c.ok('log: empty equirect band reported', bool(said) and 'upper vertical angle -0.2' in said[0],
         said[0].strip() if said else 'no line', 'equirect2 layer not drawn ... upper vertical angle -0.2')
    cap = os.environ['XRMIMIC_CAPTURE']
    cw, ch, cpx = read_bmp(cap)
    c.colour('capture: left eye', cpx(cw // 4, ch // 2), rgb(RED))
    c.colour('capture: right eye', cpx(3 * cw // 4, ch // 2), rgb(GREEN))
    return c


@scenario
def s_cube():
    """An opaque cube layer at infinity: straight ahead is its -Z face."""
    c = Checks()
    app = App()
    app.start()
    eyes = eye_swapchains(app)
    cube = Swapchain(app, 64, 64, faces=6)
    face_colours = [RED, GREEN, BLUE, YELLOW, MAGENTA, CYAN]  # +X -X +Y -Y +Z -Z

    def render(i, views, fs):
        paint_eyes(app, eyes, [BLACK, BLACK])
        cube.paint(lambda vs, t: [app.d3d.clear(v, face_colours[k]) for k, v in enumerate(vs)])
        cl = CompositionLayerCube(space=app.local.value, eyeVisibility=EYES_BOTH, swapchain=cube.handle.value,
                                  imageArrayIndex=0, orientation=Quat(0, 0, 0, 1))
        return [app.projection(eyes, views), cl]
    app.run(render, snapshot=snapshot_path())
    views = app.last_views
    app.stop()
    win = Window(snapshot_path(), app.eye_w, app.eye_h)
    for e, name in ((0, 'left'), (1, 'right')):
        c.colour(f'{name} eye: ahead is the -Z face', win.px(*win.eye_dir(e, views[e], (0, 0, -1))), rgb(CYAN))
    # 48 degrees right: past the cube's edge, only the right eye's field of view reaches it.
    c.colour('right eye: 48 degrees right is the +X face', win.px(*win.eye_dir(1, views[1], (1, 0, -0.9))), rgb(RED))
    return c


@scenario
def s_depth_view():
    """The Depth view: each eye's depth buffer as greys (1 m left, 4 m right)."""
    c = Checks()
    samples = 4 if os.environ.get('XRMIMIC_PY_MSAA') else 1
    app = App()
    app.start()
    eyes = eye_swapchains(app, samples)
    depths = [Swapchain(app, app.eye_w, app.eye_h, fmt=D32_FLOAT, samples=samples, depth=True) for _ in range(2)]

    def render(i, views, fs):
        paint_eyes(app, eyes, [RED, GREEN])
        info = [app.depth_info(depths[0], 1.0), app.depth_info(depths[1], 4.0)]
        return [app.projection(eyes, views, depth=info)]
    app.run(render, snapshot=snapshot_path())
    app.stop()
    win = Window(snapshot_path(), app.eye_w, app.eye_h)
    for e, metres in ((0, 1.0), (1, 4.0)):
        g = round(255 / (1 + metres / 2))
        for fx, fy in ((0.5, 0.5), (0.2, 0.2), (0.8, 0.85)):
            x = win.x0 + e * win.vw / 2 + fx * win.vw / 2
            c.colour(f'eye {e} depth {metres} m at ({fx}, {fy})', win.px(x, win.y0 + fy * win.vh), (g, g, g), 12)
    return c


@scenario
def s_centre():
    """The Centre view without depth: the left eye's picture on the left, the right's on the right."""
    c = Checks()
    app = App()
    app.start()
    eyes = eye_swapchains(app)

    def render(i, views, fs):
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]
    app.run(render, snapshot=snapshot_path())
    views = app.last_views
    app.stop()
    win = Window(snapshot_path(), app.eye_w, app.eye_h)
    f0, f1 = views[0][1], views[1][1]
    ct = [math.tan(min(f0.left, f1.left)), math.tan(max(f0.right, f1.right)), math.tan(max(f0.up, f1.up)), math.tan(min(f0.down, f1.down))]
    aspect = (ct[1] - ct[0]) / (ct[2] - ct[3])
    w, h = win.vw0, win.vw0 / aspect
    if h > win.vh0:
        h, w = win.vh0, win.vh0 * aspect
    x0, y0 = (win.vw0 - w) / 2, 34 + (win.vh0 - h) / 2
    at = lambda tx: (x0 + (tx - ct[0]) / (ct[1] - ct[0]) * w, y0 + h / 2)
    c.colour('centre view: left of centre from the left eye', win.px(*at(-0.4)), rgb(RED))
    c.colour('centre view: right of centre from the right eye', win.px(*at(0.4)), rgb(GREEN))
    if x0 > 30:
        c.colour('centre view: the window background beside it', win.px(x0 - 20, y0 + h / 2), (15, 18, 23), 12)
    return c


WALL, STRIPE = 0.5, 0.015


@scenario
def s_reproject():
    """Reproject: frames claim eye poses 6 cm left of the real ones, with stripes on a
    wall 0.5 m ahead where those stale eyes see them. With depth the window moves
    them to where the eyes see them now; without (XRMIMIC_PY_NODEPTH) it only turns."""
    c = Checks()
    with_depth = not os.environ.get('XRMIMIC_PY_NODEPTH')
    stale = 0.06
    stripes = [(-0.15, YELLOW), (0.15, GREEN)]
    app = App()
    app.start()
    eyes = eye_swapchains(app)
    depths = [Swapchain(app, app.eye_w, app.eye_h, fmt=D32_FLOAT, depth=True) for _ in range(2)]

    def column(view, wx):
        ps, fov = view
        q = (ps.orientation.x, ps.orientation.y, ps.orientation.z, ps.orientation.w)
        l = qrot((-q[0], -q[1], -q[2], q[3]), (wx - (ps.position.x - stale), -ps.position.y, -WALL - ps.position.z))
        tl, tr = math.tan(fov.left), math.tan(fov.right)
        return round((l[0] / -l[2] - tl) / (tr - tl) * app.eye_w)

    def render(i, views, fs):
        for e in range(2):
            def draw(v, t, e=e):
                app.d3d.clear(v, BLUE)
                for x, colour in stripes:
                    a, b = column((views[e].pose, views[e].fov), x - STRIPE), column((views[e].pose, views[e].fov), x + STRIPE)
                    app.d3d.fill(t, max(0, a), 0, min(app.eye_w, b), app.eye_h, colour)
            eyes[e].paint(draw)
        info = [app.depth_info(d, WALL) for d in depths] if with_depth else None
        return [app.projection(eyes, views, depth=info, stale=stale)]
    app.run(render, snapshot=snapshot_path())
    views = app.last_views
    app.stop()
    win = Window(snapshot_path(), app.eye_w, app.eye_h)
    for e in range(2):
        for x, colour in stripes:
            now = win.px(*win.eye_point(e, views[e], (x, -0.15, -WALL)))
            drawn = win.px(*win.eye_point(e, views[e], (x + stale, -0.15, -WALL)))
            if with_depth:
                c.colour(f'eye {e} stripe {x}: where the eye sees it now', now, rgb(colour))
                c.colour(f'eye {e} stripe {x}: gone from where it was drawn', drawn, rgb(BLUE))
            else:
                c.colour(f'eye {e} stripe {x} (no depth): where it was drawn', drawn, rgb(colour))
                c.colour(f'eye {e} stripe {x} (no depth): not moved', now, rgb(BLUE))
    return c


@scenario
def s_depth_test():
    """XR_FB_composition_layer_depth_test: a quad 1.5 m away, tested LESS against the
    projection's depth: hidden behind the left eye's 1 m, shown before the right's 4 m."""
    c = Checks()
    app = App()
    app.start()
    eyes = eye_swapchains(app)
    depths = [Swapchain(app, app.eye_w, app.eye_h, fmt=D32_FLOAT, depth=True) for _ in range(2)]
    yellow = solid(app, 64, 64, YELLOW)
    test = CompositionLayerDepthTest(depthMask=0, compareOp=1)

    def render(i, views, fs):
        paint_eyes(app, eyes, [RED, GREEN])
        info = [app.depth_info(depths[0], 1.0), app.depth_info(depths[1], 4.0)]
        q = quad(app, yellow, pose(0, 0, -1.5), 0.6)
        q.next = ctypes.addressof(test)
        return [app.projection(eyes, views, depth=info), q]
    app.run(render, snapshot=snapshot_path())
    views = app.last_views
    app.stop()
    win = Window(snapshot_path(), app.eye_w, app.eye_h)
    c.colour('left eye: quad hidden behind 1 m', win.px(*win.eye_point(0, views[0], (0, 0, -1.5))), rgb(RED))
    c.colour('right eye: quad in front of 4 m', win.px(*win.eye_point(1, views[1], (0, 0, -1.5))), rgb(YELLOW))
    return c


@scenario
def s_boundary():
    """The play-area boundary shows as a blue grid around a head near its edges
    (a 1.5 m square play area), and not at all when the device has none."""
    c = Checks()
    app = App()
    app.start()
    eyes = eye_swapchains(app)

    def render(i, views, fs):
        paint_eyes(app, eyes, [BLACK, BLACK])
        return [app.projection(eyes, views)]
    app.run(render, snapshot=snapshot_path())
    app.stop()
    win = Window(snapshot_path(), app.eye_w, app.eye_h)
    blue = 0
    for y in range(int(win.y0) + 5, int(win.y0 + win.vh) - 5, 4):
        for x in range(int(win.x0) + 5, int(win.x0 + win.vw) - 5, 4):
            p = win.px(x, y)
            if p and p[2] > 60 and p[2] > p[0] + 30:
                blue += 1
    if os.environ.get('XRMIMIC_PY_NO_PLAY_AREA'):
        c.eq('no boundary without a play area (bluish samples)', blue, 0)
    else:
        c.ok('boundary grid shows near the edge (bluish samples)', blue > 40, blue, '> 40 (0 without a play area)')
    return c


@scenario
def s_haptics_bar():
    """A vibration the app sends shows as a bar down that hand's side of the view."""
    c = Checks()
    app = App()
    app.start()
    acts = Actions(app)
    eyes = eye_swapchains(app)
    sent = []

    def render(i, views, fs):
        acts.sync()
        if not sent and acts.pulse(0, 1.0, INFINITE_DURATION) == 0:
            sent.append(i)
        paint_eyes(app, eyes, [BLACK, BLACK])
        return [app.projection(eyes, views)]
    app.run(render, snapshot=snapshot_path())
    app.stop()
    c.ok('pulse accepted', bool(sent), sent)
    win = Window(snapshot_path(), app.eye_w, app.eye_h)
    mid = win.y0 + win.vh / 2
    c.colour('left edge: orange bar (amplitude 1)', win.px(win.x0 + 3, mid), (255, 115, 13), 25)
    c.colour('right edge: no bar', win.px(win.x0 + win.vw - 3, mid), (0, 0, 0), 25)
    return c



# ---------------------------------------------------------------- the control API and the stepped clock


class Control:
    """The control API of the runtime in this process (XRMIMIC_CONTROL)."""

    def __init__(self, token=None):
        import urllib.request
        self.urllib = urllib.request
        path = os.environ['XRMIMIC_CONTROL_FILE']
        for _ in range(200):
            if os.path.exists(path) and open(path).read().strip():
                break
            time.sleep(0.01)
        self.base = 'http://' + open(path).read().strip()
        self.token = token

    def call(self, method, path, body=None, raw=False, token=None, data=None):
        import urllib.error
        if data is None and body is not None:
            data = json.dumps(body).encode()
        req = self.urllib.Request(self.base + path, data=data, method=method)
        if data is not None:
            req.add_header('Content-Type', 'application/json')
        tok = self.token if token is None else token
        if tok:
            req.add_header('Authorization', 'Bearer ' + tok)
        try:
            with self.urllib.urlopen(req, timeout=30) as r:
                payload = r.read()
                return r.status, (payload if raw else json.loads(payload))
        except urllib.error.HTTPError as e:
            payload = e.read()
            try:
                return e.code, json.loads(payload)
            except ValueError:
                return e.code, payload

    def get(self, path, **kw):
        return self.call('GET', path, **kw)

    def post(self, path, body=None, **kw):
        return self.call('POST', path, {} if body is None else body, **kw)


class Background:
    """Run a control call on another thread while this one renders frames."""

    def __init__(self, fn):
        import threading
        self.result = None
        self.thread = threading.Thread(target=self._run, args=(fn,))
        self.thread.start()

    def _run(self, fn):
        self.result = fn()

    def done(self):
        return not self.thread.is_alive()


def frames_until(app, render, done, limit=1200):
    """Render frames until done() (at least one), at most `limit`."""
    for i in range(limit):
        app.frame(lambda v, fs: render(i, v, fs))
        if done():
            return i + 1
    return limit


@scenario
def s_control():
    """The control API (XRMIMIC_CONTROL): state, head, controller inputs and placing,
    hands, haptics, captures, waiting for frames, the device, errors."""
    c = Checks()
    app = App()
    app.start()
    ctl = Control()
    acts = Actions(app)
    eyes = eye_swapchains(app)
    view = app.space(SPACE_VIEW)
    grips = [acts.grip_space(0), acts.grip_space(1)]

    def render(i, views, fs):
        acts.sync()
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]

    def run(n):
        app.run(render, frames=n)
    run(20)
    st, state = ctl.get('/v1/state')
    sess = state.get('session') or {}
    c.eq('state: 200', st, 200)
    c.eq('state: session focused', sess.get('state'), 'FOCUSED')
    c.eq('state: app name', sess.get('app'), 'xrmimic python test')
    c.eq('state: device', state['device']['name'], 'Oculus Quest2')
    c.eq('state: interaction profile', sess.get('profile'), '/interaction_profiles/oculus/touch_controller')
    c.ok('state: bindings listed', ['/user/hand/right/input/trigger/value', 'trigger'] in sess.get('bound', []), len(sess.get('bound', [])))
    c.ok('state: frames counted', sess.get('frame', 0) >= 20, sess.get('frame'))
    # Head.
    st, _ = ctl.post('/v1/head', {'position': [1.0, 1.5, -2.0], 'yaw': 0.5, 'pitch': 0.0})
    run(3)
    head = app.locate(view, app.stage).pose
    c.ok('head moved there (STAGE)', abs(head.position.x - 1.0) < 0.02 and abs(head.position.y - 1.5) < 0.02 and abs(head.position.z + 2.0) < 0.02,
         (round(head.position.x, 3), round(head.position.y, 3), round(head.position.z, 3)))
    fwd = qrot((head.orientation.x, head.orientation.y, head.orientation.z, head.orientation.w), (0, 0, -1))
    c.near('head turned 0.5 rad left', math.atan2(-fwd[0], -fwd[2]), 0.5, 0.01)
    # Controller inputs: held, merged, replaced.
    ctl.post('/v1/controller/right', {'input': {'trigger': 0.7, 'button_lower': True}})
    run(5)
    c.near('held right trigger reaches the action', acts.float(acts.trigger, 1), 0.7, 0.011)
    c.ok('held A reaches the action', acts.bool(acts.primary, 1))
    ctl.post('/v1/controller/right', {'input': {'squeeze': 1.0}})
    run(5)
    c.ok('a later request adds to what is held', abs(acts.float(acts.trigger, 1) - 0.7) < 0.011 and abs(acts.float(acts.squeeze, 1) - 1.0) < 0.011,
         (acts.float(acts.trigger, 1), acts.float(acts.squeeze, 1)))
    ctl.post('/v1/controller/right', {'input': {}, 'replace': True})
    run(5)
    c.near('replace: nothing held', acts.float(acts.trigger, 1), 0.0, 0.011)
    st, bad = ctl.post('/v1/controller/right', {'input': {'trigga': 1}})
    c.eq('unknown input refused', st, 400)
    # Placing a controller.
    ctl.post('/v1/controller/left', {'put': {'position': [0.2, 1.0, -0.4]}})
    run(3)
    g = app.locate(grips[0], app.stage).pose.position
    c.ok('left controller put down there', abs(g.x - 0.2) < 0.01 and abs(g.y - 1.0) < 0.01 and abs(g.z + 0.4) < 0.01, (round(g.x, 3), round(g.y, 3), round(g.z, 3)))
    ctl.post('/v1/controller/left', {'put': None})
    run(3)
    g = app.locate(grips[0], app.stage).pose.position
    c.ok('given back to the head', abs(g.z + 0.4) > 0.05 or abs(g.x - 0.2) > 0.05, (round(g.x, 3), round(g.y, 3), round(g.z, 3)))
    # Haptics, as the app sent them.
    acts.pulse(1, 0.6, 50_000_000)
    _, state = ctl.get('/v1/state')
    hr = state['session']['haptics'][1]
    c.ok('haptics reported', hr['count'] == 1 and abs(hr['last']['amplitude'] - 0.6) < 1e-3, hr)
    # Waiting for a frame while frames run.
    target = state['session']['frame'] + 30
    w = Background(lambda: ctl.get(f'/v1/wait?frame={target}&timeout_ms=10000'))
    frames_until(app, render, w.done)
    w.thread.join()
    st, wr = w.result
    c.ok('wait for a frame', st == 200 and wr['ok'] and wr['frame'] >= target, (st, wr))
    st, wr = ctl.get('/v1/wait?state=STOPPING&timeout_ms=50')
    c.ok('a wait that times out', st == 408 and not wr['ok'], (st, wr))
    # Eye capture: the next frame, as the app drew it.
    w = Background(lambda: ctl.get('/v1/capture/eyes?format=bmp', raw=True))
    frames_until(app, render, w.done)
    w.thread.join()
    st, bmp = w.result
    if st == 200:
        path = os.path.join(os.getcwd(), 'eyes.bmp')
        open(path, 'wb').write(bmp)
        ew, eh, epx = read_bmp(path)
        c.colour('eye capture: left eye', epx(ew // 4, eh // 2), rgb(RED))
        c.colour('eye capture: right eye', epx(3 * ew // 4, eh // 2), rgb(GREEN))
    else:
        c.ok('eye capture', False, (st, bmp[:200]))
    # Window capture as PNG.
    w = Background(lambda: ctl.get('/v1/capture/window?format=png', raw=True))
    frames_until(app, render, w.done)
    w.thread.join()
    st, png = w.result
    c.ok('window capture is a PNG', st == 200 and png[:8] == b'\x89PNG\r\n\x1a\n', (st, png[:16]))
    # Views.
    st, _ = ctl.post('/v1/views', {'centre': True})
    c.eq('views', st, 200)
    # Hands.
    ctl.post('/v1/hands', {'on': True})
    run(10)
    c.eq('hands: the hand profile', acts.profile(1), '/interaction_profiles/ext/hand_interaction_ext')
    ctl.post('/v1/hands', {'on': False})
    run(10)
    c.eq('hands off: Touch again', acts.profile(1), '/interaction_profiles/oculus/touch_controller')
    # The device: a preset; the play area changes, the app is told.
    st, d = ctl.post('/v1/device', {'preset': 'index'})
    c.ok('device preset', st == 200 and d['device'] == 'Valve Index', (st, d))
    run(10)
    c.ok('play-area change event', any(e[0] == 'space_change' for e in app.events), app.events[-4:])
    st, d = ctl.post('/v1/device', {'settings': 'play_area_m=banana'})
    c.eq('bad device settings refused', st, 400)
    # Errors.
    c.eq('unknown request', ctl.get('/v1/nope')[0], 404)
    c.eq('body not JSON refused', ctl.call('POST', '/v1/head', data=b'{not json')[0], 400)
    app.stop()
    return c


@scenario
def s_control_token():
    """XRMIMIC_CONTROL_TOKEN: requests need it."""
    c = Checks()
    app = App()
    app.start()
    eyes = eye_swapchains(app)
    app.run(lambda i, v, fs: (paint_eyes(app, eyes, [RED, GREEN]), [app.projection(eyes, v)])[1], frames=5)
    ctl = Control()
    c.eq('without the token: 401', ctl.get('/v1/state', token='')[0], 401)
    c.eq('wrong token: 401', ctl.get('/v1/state', token='nope')[0], 401)
    c.eq('with the token: 200', ctl.get('/v1/state', token='s3cret')[0], 200)
    app.stop()
    return c


@scenario
def s_stepped():
    """XRMIMIC_CLOCK=step: display times one period apart, frames faster than real
    time, the user moves exactly one period per frame; pause, step, resume."""
    c = Checks()
    app = App()
    app.start()
    ctl = Control()
    eyes = eye_swapchains(app)
    view = app.space(SPACE_VIEW)
    times = []

    def render(i, views, fs):
        times.append(fs.predictedDisplayTime)
        paint_eyes(app, eyes, [RED, GREEN])
        return [app.projection(eyes, views)]
    start = app.locate(view, app.stage).pose.position
    t0 = time.perf_counter()
    n = 180
    app.run(render, frames=n)
    wall = time.perf_counter() - t0
    period = round(1e9 / 90)
    steps = {b - a for a, b in zip(times, times[1:])}
    c.ok('display times exactly one period apart', steps == {period} or steps == {period - 1, period, period + 1}, sorted(steps)[:5])
    end = app.locate(view, app.stage).pose.position
    walked = start.z - end.z
    # Walking starts with the first frame; xrWaitFrame steps the user before each frame.
    c.near('walked exactly frames x period x 1.4 m/s (m)', walked, n * period / 1e9 * 1.4, 0.001)
    c.ok('frames do not wait for real time (this Python app is the limit)', True, f'{n} frames in {wall:.2f} s')
    _, state = ctl.get('/v1/state')
    c.ok('state: stepped clock', state['session']['clock']['stepped'], state['session']['clock'])
    # Pause, step, resume: the frame loop runs on another thread now.
    import threading
    stop = threading.Event()
    counted = []

    def loop():
        i = 0
        while not stop.is_set():
            app.frame(lambda v, fs: render(i, v, fs))
            counted.append(1)
            i += 1
    t = threading.Thread(target=loop)
    t.start()
    time.sleep(0.2)
    ctl.post('/v1/clock', {'paused': True})
    time.sleep(0.2)
    f0 = ctl.get('/v1/state')[1]['session']['frame']
    time.sleep(0.3)
    f1 = ctl.get('/v1/state')[1]['session']['frame']
    c.ok('paused: no frames', f1 - f0 <= 1, (f0, f1))
    st, r = ctl.post('/v1/clock', {'step': 10, 'wait': True})
    c.ok('step 10: exactly ten more frames', st == 200 and r['frame'] - f1 in (10, 11), (f1, r))
    time.sleep(0.2)
    f2 = ctl.get('/v1/state')[1]['session']['frame']
    c.ok('still paused after the steps', f2 - r['frame'] <= 1, (r['frame'], f2))
    ctl.post('/v1/clock', {'paused': False})
    time.sleep(0.3)
    f3 = ctl.get('/v1/state')[1]['session']['frame']
    c.ok('resumed: frames run again', f3 - f2 > 10, (f2, f3))
    stop.set()
    t.join(10)
    app.stop()
    return c

# ---------------------------------------------------------------- running scenarios

def run_scenario(name):
    fn = SCENARIOS[name]
    try:
        checks = fn()
        out = {'checks': checks.items}
    except Exception as e:  # noqa: BLE001 - reported to the parent as a failed check
        import traceback
        out = {'checks': [], 'error': f'{e}\n{traceback.format_exc()}'}
    print('RESULT ' + json.dumps(out), flush=True)


# The environment of each test: scenario, extra variables, whether the window
# photographs itself (then it is shown; otherwise hidden).
TESTS = {
    'api': ('api', {'XRMIMIC_TEST_CONTROLLERS': '1'}, False),
    'motion': ('motion', {'XRMIMIC_TEST_MOVE': '1', 'XRMIMIC_TEST_TURN': '0.5'}, False),
    'placed_controller': ('placed', {'XRMIMIC_TEST_HAND_PUT': '0.3,1.0,-0.5', 'XRMIMIC_TEST_MOVE': '1'}, False),
    'input_tab': ('input_tab', {'XRMIMIC_TEST_TAB': 'input',
                                'XRMIMIC_TEST_INPUT': 'right trigger/value=0.7; right a/click=1; left thumbstick=0.5,-0.25'}, False),
    'hands': ('hands', {'XRMIMIC_HANDS': '1', 'XRMIMIC_TEST_TAB': 'input', 'XRMIMIC_TEST_INPUT': 'right pinch_ext/value=1'}, False),
    'device_preset': ('device', {'XRMIMIC_DEVICE': 'index'}, False),
    'play_area_change': ('stage_change', {'XRMIMIC_TEST_DEVICE_APPLY': 'index'}, False),
    'layers': ('layers', {'XRMIMIC_CAPTURE_FRAME': '60'}, True),
    'layers_msaa': ('layers', {'XRMIMIC_PY_MSAA': '1', 'XRMIMIC_CAPTURE_FRAME': '60'}, True),
    'cube_layer': ('cube', {}, True),
    'depth_view': ('depth_view', {'XRMIMIC_TEST_DEPTH_VIEW': '1'}, True),
    'depth_view_msaa': ('depth_view', {'XRMIMIC_TEST_DEPTH_VIEW': '1', 'XRMIMIC_PY_MSAA': '1'}, True),
    'centre_view': ('centre', {'XRMIMIC_TEST_CENTRE_VIEW': '1'}, True),
    'reproject': ('reproject', {'XRMIMIC_TEST_REPROJECT': '1'}, True),
    'reproject_nodepth': ('reproject', {'XRMIMIC_TEST_REPROJECT': '1', 'XRMIMIC_PY_NODEPTH': '1'}, True),
    'depth_tested_layer': ('depth_test', {}, True),
    'boundary': ('boundary', {'XRMIMIC_PY_PLAY_AREA': '1.5x1.5'}, True),
    'boundary_without_play_area': ('boundary', {'XRMIMIC_PY_PLAY_AREA': '0', 'XRMIMIC_PY_NO_PLAY_AREA': '1'}, True),
    'haptics_bar': ('haptics_bar', {}, True),
    'control': ('control', {'XRMIMIC_CONTROL': '127.0.0.1:0'}, True),
    'control_token': ('control_token', {'XRMIMIC_CONTROL': '127.0.0.1:0', 'XRMIMIC_CONTROL_TOKEN': 's3cret'}, False),
    'stepped_clock': ('stepped', {'XRMIMIC_CONTROL': '127.0.0.1:0', 'XRMIMIC_CLOCK': 'step', 'XRMIMIC_TEST_MOVE': '1'}, False),
}


def child(test):
    scen, extra, picture = TESTS[test]
    work = tempfile.mkdtemp(prefix=f'xrmimic-py-{test}-')
    # A clean XRMimic environment (no device file, preset or test hooks from
    # the caller's shell), but the same runtime as this process.
    env = {k: v for k, v in os.environ.items() if not k.startswith('XRMIMIC_')}
    env['XRMIMIC_TEST_DLL'] = DLL
    env.update(extra)
    env['XRMIMIC_LOG'] = os.path.join(work, 'xrmimic.log')
    env['XRMIMIC_CAPTURE'] = os.path.join(work, 'capture.bmp')
    if 'XRMIMIC_CONTROL' in env:
        env['XRMIMIC_CONTROL_FILE'] = os.path.join(work, 'control.txt')
    if picture:
        env['XRMIMIC_TEST_SNAPSHOT'] = os.path.join(work, 'window.bmp')
    else:
        env['XRMIMIC_HIDDEN'] = '1'
    if env.get('XRMIMIC_PY_PLAY_AREA'):
        # The device's play area through a settings file (as the Device tab saves it).
        dev = os.path.join(work, 'device.ini')
        with open(dev, 'w') as fh:
            fh.write(f'play_area_m={env["XRMIMIC_PY_PLAY_AREA"]}\n')
        env['XRMIMIC_DEVICE_FILE'] = dev
    p = subprocess.run([sys.executable, os.path.abspath(__file__), '--scenario', scen], env=env, cwd=work,
                       capture_output=True, text=True, timeout=180)
    line = next((l for l in reversed(p.stdout.splitlines()) if l.startswith('RESULT ')), None)
    if line is None:
        return {'checks': [], 'error': f'no result (exit {p.returncode})\n{p.stdout[-2000:]}\n{p.stderr[-2000:]}'}, work
    return json.loads(line[7:]), work


class XRMimicTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if sys.platform != 'win32':
            raise unittest.SkipTest('XRMimic\'s D3D11 runtime: Windows only')
        if not os.path.exists(DLL):
            raise unittest.SkipTest(f'{DLL} not built (cargo build --release)')


def _make(test):
    def t(self):
        out, work = child(test)
        if out.get('error'):
            self.fail(f'{test}: {out["error"]}\n(work folder {work})')
        failed = [c for c in out['checks'] if not c[1]]
        for what, good, got, want in out['checks']:
            if os.environ.get('XRMIMIC_PY_VERBOSE'):
                print(f'  {"ok  " if good else "FAIL"} {what}: {got} (want {want})')
        self.assertTrue(out['checks'], f'{test}: no checks ran')
        self.assertFalse(failed, f'{test}: ' + '; '.join(f'{w}: got {g}, want {x}' for w, _, g, x in failed) + f' (work folder {work})')
    t.__doc__ = (SCENARIOS[TESTS[test][0]].__doc__ or '').strip().splitlines()[0]
    return t


for _name in TESTS:
    setattr(XRMimicTest, f'test_{_name}', _make(_name))


if __name__ == '__main__':
    if len(sys.argv) >= 3 and sys.argv[1] == '--scenario':
        run_scenario(sys.argv[2])
    else:
        unittest.main()
