public class Person {
  int age;
  String name;

  Person(int age, String name) {
    this.age = age;
    this.name = name;
  }

  public void sayHello() {
    System.out.println("Hello, my name is " + name);
  }
}
