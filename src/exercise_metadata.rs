use serde_json::{Map, Value, json};

use crate::lyfta::numeric_id;

// Keep each Lyfta ID, Rust variant and display label together.
macro_rules! lyfta_mapping {
    ($name:ident, $kind:literal, { $($id:literal => $variant:ident: $label:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name {
            $($variant,)+
            Unknown(u64),
        }

        impl From<u64> for $name {
            fn from(id: u64) -> Self {
                match id {
                    $($id => Self::$variant,)+
                    id => Self::Unknown(id),
                }
            }
        }

        impl $name {
            pub fn label(self) -> String {
                match self {
                    $(Self::$variant => $label.into(),)+
                    Self::Unknown(id) => format!(concat!("Lyfta ", $kind, " #{id}"), id = id),
                }
            }
        }
    };
}

lyfta_mapping!(Equipment, "equipment", {
    1 => Barbell: "Barbell",
    2 => BodyWeight: "Body weight",
    3 => Cable: "Cable",
    4 => Dumbbell: "Dumbbell",
    5 => EzBarbell: "EZ Barbell",
    6 => LeverageMachine: "Leverage machine",
    7 => SledMachine: "Sled machine",
    8 => SmithMachine: "Smith machine",
    9 => Weighted: "Weighted",
    10 => Assisted: "Assisted",
    11 => Band: "Band",
    12 => BattlingRope: "Battling Rope",
    13 => BosuBall: "Bosu ball",
    14 => Hammer: "Hammer",
    15 => Kettlebell: "Kettlebell",
    16 => MedicineBall: "Medicine Ball",
    17 => OlympicBarbell: "Olympic barbell",
    18 => PowerSled: "Power Sled",
    19 => ResistanceBand: "Resistance Band",
    20 => Roll: "Roll",
    21 => Rollball: "Rollball",
    22 => Rope: "Rope",
    23 => StabilityBall: "Stability ball",
    24 => Stick: "Stick",
    25 => Suspension: "Suspension",
    26 => TrapBar: "Trap bar",
    27 => VibratePlate: "Vibrate Plate",
    28 => WheelRoller: "Wheel roller",
});

lyfta_mapping!(BodyPart, "body part", {
    1 => Thighs: "Thighs",
    2 => Chest: "Chest",
    3 => Hips: "Hips",
    4 => Back: "Back",
    5 => UpperArms: "Upper Arms",
    6 => Shoulders: "Shoulders",
    7 => Forearms: "Forearms",
    8 => Calves: "Calves",
    9 => Neck: "Neck",
    10 => Cardio: "Cardio",
    11 => FullBody: "Full body",
    12 => Waist: "Waist",
    13 => Plyometrics: "Plyometrics",
    14 => Weightlifting: "Weightlifting",
    15 => Yoga: "Yoga",
    16 => Stretching: "Stretching",
    17 => Biceps: "Biceps",
    18 => Triceps: "Triceps",
    19 => Quadriceps: "Quadriceps",
    20 => Hamstrings: "Hamstrings",
});

lyfta_mapping!(Muscle, "muscle", {
    2 => AdductorLongus: "Adductor Longus",
    3 => AdductorMagnus: "Adductor Magnus",
    4 => BicepsBrachii: "Biceps Brachii",
    5 => Brachialis: "Brachialis",
    6 => Brachioradialis: "Brachioradialis",
    7 => DeepHipExternalRotators: "Deep Hip External Rotators",
    8 => DeltoidAnterior: "Deltoid Anterior",
    9 => DeltoidLateral: "Deltoid Lateral",
    10 => DeltoidPosterior: "Deltoid Posterior",
    11 => ErectorSpinae: "Erector Spinae",
    12 => Gastrocnemius: "Gastrocnemius",
    13 => GluteusMaximus: "Gluteus Maximus",
    14 => GluteusMedius: "Gluteus Medius",
    15 => GluteusMinimus: "Gluteus Minimus",
    16 => Gracilis: "Gracilis",
    17 => Hamstrings: "Hamstrings",
    18 => Iliopsoas: "Iliopsoas",
    19 => Infraspinatus: "Infraspinatus",
    20 => LatissimusDorsi: "Latissimus Dorsi",
    21 => LevatorScapulae: "Levator Scapulae",
    22 => Obliques: "Obliques",
    23 => Pectineous: "Pectineous",
    24 => PectoralisMajorClavicularHead: "Pectoralis Major Clavicular Head",
    25 => PectoralisMajorSternalHead: "Pectoralis Major Sternal Head",
    26 => Popliteus: "Popliteus",
    27 => Quadriceps: "Quadriceps",
    28 => RectusAbdominis: "Rectus Abdominis",
    29 => Sartorius: "Sartorius",
    30 => SerratusAnte: "Serratus Ante",
    31 => SerratusAnterior: "Serratus Anterior",
    32 => Soleus: "Soleus",
    33 => Splenius: "Splenius",
    34 => Sternocleidomastoid: "Sternocleidomastoid",
    35 => Subscapularis: "Subscapularis",
    36 => TensorFasciaeLatae: "Tensor Fasciae Latae",
    37 => TeresMajor: "Teres Major",
    38 => TeresMinor: "Teres Minor",
    39 => TibialisAnterior: "Tibialis Anterior",
    40 => TransverseAbdominis: "Transverse Abdominis",
    41 => TrapeziusLowerFibers: "Trapezius Lower Fibers",
    42 => TrapeziusMiddleFibers: "Trapezius Middle Fibers",
    43 => TrapeziusUpperFibers: "Trapezius Upper Fibers",
    44 => TricepsBrachii: "Triceps Brachii",
    45 => WristExtensors: "Wrist Extensors",
    46 => WristFlexors: "Wrist Flexors",
});

fn ids(value: &Value) -> Vec<u64> {
    let decoded = value
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    decoded
        .as_ref()
        .unwrap_or(value)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(numeric_id)
        .collect()
}

pub fn body_parts(value: &Value) -> impl Iterator<Item = BodyPart> {
    ids(value).into_iter().map(BodyPart::from)
}

pub fn labels(exercise: &Value, fallback: &Value) -> Map<String, Value> {
    let field = |name| {
        exercise
            .get(name)
            .filter(|value| !value.is_null())
            .unwrap_or(&fallback[name])
    };
    let equipment: Vec<_> = ids(field("equipment_id"))
        .into_iter()
        .map(|id| Equipment::from(id).label())
        .collect();
    let body_parts: Vec<_> = body_parts(field("body_part_id"))
        .map(BodyPart::label)
        .collect();
    let muscles = |name| {
        ids(field(name))
            .into_iter()
            .map(|id| Muscle::from(id).label())
            .collect::<Vec<_>>()
    };
    Map::from_iter([
        ("equipment".into(), json!(equipment)),
        ("body_parts".into(), json!(body_parts)),
        ("target_muscles".into(), json!(muscles("Target_muscles_id"))),
        (
            "synergist_muscles".into(),
            json!(muscles("Synergist_muscles_id")),
        ),
    ])
}
