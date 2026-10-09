import styles from "./SkeletonPage.module.css";

type Props = {
  rows?: number;
};

export default function SkeletonPage({ rows = 6 }: Props) {
  return (
    <div className={styles.wrap}>
      <div className={styles.head}>
        <div className={`${styles.bar} ${styles.title}`} />
        <div className={`${styles.bar} ${styles.subtitle}`} />
      </div>
      <div className={styles.rows}>
        {Array.from({ length: rows }, (_, i) => (
          <div key={i} className={`${styles.bar} ${styles.row}`} />
        ))}
      </div>
    </div>
  );
}
