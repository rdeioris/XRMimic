//! XR_EXT_hand_tracking: hand trackers and their 26 joints, from the
//! simulated hands (xrmimic_core::hands). Joints are reported only while the
//! session uses hands (the Input tab's Hands, or the H key) and that hand is
//! not out of view.

use crate::api::{find_in_chain, space_pose};
use crate::state::rt;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use xr::Handle as _;
use xrmimic_core::hands::{hand_joints, pinch_point, wrist_pose, JOINT_COUNT};
use xrmimic_core::math::Pose;
use xrmimic_core::sim::{SimState, LEFT, RIGHT};

use openxr_sys as xr;

/// Hand tracker handle -> (session, hand).
static TRACKERS: LazyLock<Mutex<HashMap<u64, (u64, usize)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The joints of `hand` in STAGE space, if that hand is tracked now.
pub fn stage_joints(sim: &SimState, hands: bool, hand: usize) -> Option<[(Pose, f32); JOINT_COUNT]> {
    let c = &sim.controllers[hand];
    if !hands || c.hand_hidden {
        return None;
    }
    let wrist = wrist_pose(hand, &sim.aim_pose(hand));
    Some(hand_joints(hand, &c.hand_shape()).map(|j| (wrist.compose(&j.pose), j.radius)))
}

/// XR_EXT_hand_interaction's pinch pose (between thumb and index tips, facing
/// like the aim) and poke pose (the index fingertip), in STAGE space.
pub fn pinch_and_poke(sim: &SimState, hand: usize) -> (Pose, Pose) {
    let aim = sim.aim_pose(hand);
    let wrist = wrist_pose(hand, &aim);
    let joints = hand_joints(hand, &sim.controllers[hand].hand_shape());
    let pinch = Pose::new(aim.orientation, wrist.transform_point(pinch_point(&joints)));
    let poke = wrist.compose(&joints[xrmimic_core::hands::INDEX_TIP].pose);
    (pinch, poke)
}

fn extension_enabled(instance: u64) -> bool {
    rt().instances.get(&instance).is_some_and(|i| i.extensions.iter().any(|e| e == "XR_EXT_hand_tracking"))
}

pub unsafe extern "system" fn create_hand_tracker(
    session: xr::Session,
    info: *const xr::HandTrackerCreateInfoEXT,
    out: *mut xr::HandTrackerEXT,
) -> xr::Result {
    crate::traced!("xrCreateHandTrackerEXT", [session, info, out], || {
        let s = session.into_raw();
        let Some(instance) = rt().sessions.get(&s).map(|d| d.instance) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if !extension_enabled(instance) {
            return xr::Result::ERROR_FUNCTION_UNSUPPORTED;
        }
        let info = &*info;
        let hand = match info.hand {
            xr::HandEXT::LEFT => LEFT,
            xr::HandEXT::RIGHT => RIGHT,
            _ => return xr::Result::ERROR_VALIDATION_FAILURE,
        };
        if info.hand_joint_set != xr::HandJointSetEXT::DEFAULT {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        }
        let h = rt().new_handle();
        TRACKERS.lock().unwrap().insert(h, (s, hand));
        *out = xr::HandTrackerEXT::from_raw(h);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn destroy_hand_tracker(tracker: xr::HandTrackerEXT) -> xr::Result {
    crate::traced!("xrDestroyHandTrackerEXT", [tracker], || {
        if TRACKERS.lock().unwrap().remove(&tracker.into_raw()).is_some() {
            xr::Result::SUCCESS
        } else {
            xr::Result::ERROR_HANDLE_INVALID
        }
    })
}

pub unsafe extern "system" fn locate_hand_joints(
    tracker: xr::HandTrackerEXT,
    info: *const xr::HandJointsLocateInfoEXT,
    out: *mut xr::HandJointLocationsEXT,
) -> xr::Result {
    crate::traced!("xrLocateHandJointsEXT", [tracker, info, out], || {
        let Some((session, hand)) = TRACKERS.lock().unwrap().get(&tracker.into_raw()).copied() else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        let out = &mut *out;
        if out.joint_count as usize != JOINT_COUNT || out.joint_locations.is_null() {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        }
        let rt = rt();
        let Some(s) = rt.sessions.get(&session) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        let base = space_pose(&rt, (*info).base_space.into_raw());
        let hands = s.headset.hands();
        let joints = stage_joints(&s.headset.user(), hands, hand);
        let locations = std::slice::from_raw_parts_mut(out.joint_locations, JOINT_COUNT);
        let tracked = xr::SpaceLocationFlags::ORIENTATION_VALID
            | xr::SpaceLocationFlags::POSITION_VALID
            | xr::SpaceLocationFlags::ORIENTATION_TRACKED
            | xr::SpaceLocationFlags::POSITION_TRACKED;
        match (joints, base) {
            (Some(joints), Some(base)) => {
                out.is_active = xr::TRUE;
                for (l, (pose, radius)) in locations.iter_mut().zip(joints.iter()) {
                    let p = Pose::relative(&base, pose);
                    l.location_flags = tracked;
                    l.pose = xr::Posef {
                        orientation: xr::Quaternionf {
                            x: p.orientation.x,
                            y: p.orientation.y,
                            z: p.orientation.z,
                            w: p.orientation.w,
                        },
                        position: xr::Vector3f { x: p.position.x, y: p.position.y, z: p.position.z },
                    };
                    l.radius = *radius;
                }
            }
            _ => {
                out.is_active = xr::FALSE;
                for l in locations.iter_mut() {
                    l.location_flags = xr::SpaceLocationFlags::EMPTY;
                }
            }
        }
        // Velocities, if asked for: not known.
        let v = find_in_chain(out.next, xr::StructureType::HAND_JOINT_VELOCITIES_EXT) as *mut xr::HandJointVelocitiesEXT;
        if !v.is_null() && !(*v).joint_velocities.is_null() {
            let n = ((*v).joint_count as usize).min(JOINT_COUNT);
            for jv in std::slice::from_raw_parts_mut((*v).joint_velocities, n) {
                jv.velocity_flags = xr::SpaceVelocityFlags::EMPTY;
            }
        }
        xr::Result::SUCCESS
    })
}
